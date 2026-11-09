// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
#![cfg(feature = "async-wait")]

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;
use qubit_rayon_executor::RayonTaskHandle;

const DEADLINE: Duration = Duration::from_secs(10);

fn service_with_blocker() -> (RayonExecutorService, RayonTaskHandle<(), io::Error>, mpsc::Sender<()>) {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocker = service
        .submit_tracked(move || {
            started_tx.send(()).expect("coordinator should receive start");
            release_rx
                .recv_timeout(DEADLINE)
                .map_err(|error| io::Error::other(error.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("blocker should be accepted");
    started_rx.recv_timeout(DEADLINE).expect("worker should start blocker");
    (service, blocker, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_multiple_capacity_waiters_retry_until_accepted() {
    tokio::time::timeout(DEADLINE, async {
        let (service, blocker, release_tx) = service_with_blocker();
        let completions = Arc::new((0..4).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
        let (saturated_tx, mut saturated_rx) = tokio::sync::mpsc::channel(4);
        let (delay_tx, delay_rx) = tokio::sync::oneshot::channel();
        let mut delay_rx = Some(delay_rx);
        let (accepted_tx, mut accepted_rx) = tokio::sync::mpsc::channel(4);
        let mut producers = Vec::new();

        for id in 0..4 {
            let service = service.clone();
            let completions = Arc::clone(&completions);
            let saturated_tx = saturated_tx.clone();
            let accepted_tx = accepted_tx.clone();
            let delay_rx = if id == 0 { delay_rx.take() } else { None };
            producers.push(tokio::spawn(async move {
                // Subscribe before the first attempt so a change cannot be
                // missed.
                let mut changes = service.capacity_changes();
                let mut delayed_wait = delay_rx;
                let mut reported_saturated = false;
                loop {
                    let task_count = Arc::clone(&completions);
                    let result = service.submit_tracked_callable(move || {
                        task_count[id].fetch_add(1, Ordering::SeqCst);
                        Ok::<usize, io::Error>(id)
                    });
                    match result {
                        Ok(handle) => {
                            accepted_tx.send(id).await.expect("accepted ID should be reported");
                            let value = handle.await.expect("accepted task should finish");
                            return (id, value);
                        }
                        Err(SubmissionError::Saturated) => {
                            if !reported_saturated {
                                saturated_tx.send(id).await.expect("saturated ID should be reported");
                                reported_saturated = true;
                            }
                            if let Some(receiver) = delayed_wait.take() {
                                receiver.await.expect("coordinator should allow delayed wait");
                            }
                            tokio::time::timeout(DEADLINE, changes.changed())
                                .await
                                .expect("capacity notification should arrive")
                                .expect("capacity notification channel should remain open");
                        }
                        Err(SubmissionError::Shutdown) => {
                            panic!("service shut down during completion scenario")
                        }
                        Err(error) => panic!("unexpected submission error: {error:?}"),
                    }
                }
            }));
        }
        drop(saturated_tx);
        drop(accepted_tx);

        let mut first_saturated = Vec::new();
        for _ in 0..4 {
            first_saturated.push(
                tokio::time::timeout(DEADLINE, saturated_rx.recv())
                    .await
                    .expect("all waiters should reach saturation")
                    .expect("waiter should report saturation"),
            );
        }
        first_saturated.sort_unstable();
        assert_eq!(first_saturated, vec![0, 1, 2, 3]);
        let mut coordinator_changes = service.capacity_changes();
        // The controlled worker has completed, so the watch change is already
        // published before producer 0 is allowed to call changed().
        release_tx.send(()).expect("blocker should be released");
        let blocker_result = tokio::task::spawn_blocking(move || blocker.get())
            .await
            .expect("blocker wait should not panic");
        blocker_result.expect("blocker should complete");
        tokio::time::timeout(DEADLINE, coordinator_changes.changed())
            .await
            .expect("completion should publish capacity change")
            .expect("capacity notification channel should remain open");
        delay_tx.send(()).expect("delayed waiter should resume");

        let mut accepted = Vec::new();
        for _ in 0..4 {
            accepted.push(
                tokio::time::timeout(DEADLINE, accepted_rx.recv())
                    .await
                    .expect("each ID should eventually be accepted")
                    .expect("producer should report accepted ID"),
            );
        }
        accepted.sort_unstable();
        assert_eq!(accepted, vec![0, 1, 2, 3]);

        // Also wait for task results, with the whole orchestration bounded
        // above.
        for producer in producers {
            let (id, result) = producer.await.expect("producer should not panic");
            assert_eq!(result, id);
        }
        for (id, count) in completions.iter().enumerate() {
            assert_eq!(count.load(Ordering::SeqCst), 1, "task ID {id}");
        }
        service.shutdown();
        tokio::time::timeout(DEADLINE, service.await_termination())
            .await
            .expect("service should terminate before the final snapshot");
        let stats = service.stats();
        assert_eq!((stats.queued, stats.running, stats.cancelling), (0, 0, 0));
    })
    .await
    .expect("completion waiter scenario should finish within deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_releases_saturated_retry_waiters() {
    tokio::time::timeout(DEADLINE, async {
        let (service, blocker, release_tx) = service_with_blocker();
        let (saturated_tx, mut saturated_rx) = tokio::sync::mpsc::channel(4);
        let (outcome_tx, mut outcome_rx) = tokio::sync::mpsc::channel(4);
        let mut producers = Vec::new();
        for id in 0..4 {
            let service = service.clone();
            let saturated_tx = saturated_tx.clone();
            let outcome_tx = outcome_tx.clone();
            producers.push(tokio::spawn(async move {
                let mut changes = service.capacity_changes();
                let mut reported_saturated = false;
                loop {
                    let result = service.submit_tracked_callable(move || Ok::<usize, io::Error>(id));
                    match result {
                        Ok(handle) => {
                            let value = handle.await.expect("accepted work should complete");
                            outcome_tx
                                .send((id, Ok(value)))
                                .await
                                .expect("outcome should be reported");
                            return;
                        }
                        Err(SubmissionError::Saturated) => {
                            if !reported_saturated {
                                saturated_tx.send(id).await.expect("saturation should be reported");
                                reported_saturated = true;
                            }
                            tokio::time::timeout(DEADLINE, changes.changed())
                                .await
                                .expect("shutdown notification should arrive")
                                .expect("watch receiver should stay open");
                        }
                        Err(SubmissionError::Shutdown) => {
                            outcome_tx
                                .send((id, Err(())))
                                .await
                                .expect("shutdown should be reported");
                            return;
                        }
                        Err(error) => panic!("unexpected submission error: {error:?}"),
                    }
                }
            }));
        }
        drop(saturated_tx);
        drop(outcome_tx);
        let mut saturated = Vec::new();
        for _ in 0..4 {
            saturated.push(
                tokio::time::timeout(DEADLINE, saturated_rx.recv())
                    .await
                    .expect("all waiters should saturate before shutdown")
                    .expect("waiter should report saturation"),
            );
        }
        saturated.sort_unstable();
        assert_eq!(saturated, vec![0, 1, 2, 3]);
        service.shutdown();
        let mut outcomes = Vec::new();
        for _ in 0..4 {
            outcomes.push(
                tokio::time::timeout(DEADLINE, outcome_rx.recv())
                    .await
                    .expect("waiter should leave retry loop")
                    .expect("waiter should report shutdown"),
            );
        }
        outcomes.sort_by_key(|(id, _)| *id);
        for (expected_id, (id, outcome)) in outcomes.into_iter().enumerate() {
            assert_eq!(id, expected_id);
            assert!(outcome.is_err(), "saturated retry must observe shutdown");
        }
        for producer in producers {
            producer.await.expect("producer should not panic");
        }
        release_tx
            .send(())
            .expect("blocker should be released after waiters exit");
        let blocker_result = tokio::task::spawn_blocking(move || blocker.get())
            .await
            .expect("blocker wait should not panic");
        blocker_result.expect("blocker should complete");
        service.await_termination().await;
        assert_eq!(
            (
                service.stats().queued,
                service.stats().running,
                service.stats().cancelling
            ),
            (0, 0, 0)
        );
    })
    .await
    .expect("shutdown waiter scenario should finish within deadline");
}
