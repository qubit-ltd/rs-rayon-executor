// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::TaskResult;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;
use qubit_rayon_executor::RayonTaskHandle;

const DEADLINE: Duration = Duration::from_secs(10);

fn wait_for_result_until<R: Send, E: Send>(mut handle: RayonTaskHandle<R, E>, deadline: Instant) -> TaskResult<R, E> {
    loop {
        match handle.try_get() {
            qubit_executor::TryGet::Ready(result) => return result,
            qubit_executor::TryGet::Pending(pending) => {
                handle = pending;
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "task result exceeded its deadline");
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}

#[test]
fn test_concurrent_producers_respect_capacity() {
    let service = RayonExecutorService::builder()
        .num_threads(2)
        .task_capacity(6)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let blockers = (0..2)
        .map(|_| {
            let started_tx = started_tx.clone();
            let release_rx = Arc::clone(&release_rx);
            service
                .submit_tracked(move || {
                    started_tx.send(()).expect("coordinator should receive start");
                    release_rx
                        .lock()
                        .expect("release lock should not be poisoned")
                        .recv_timeout(DEADLINE)
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    Ok::<(), io::Error>(())
                })
                .expect("blocker should be accepted")
        })
        .collect::<Vec<_>>();
    for _ in 0..2 {
        started_rx
            .recv_timeout(DEADLINE)
            .expect("both workers should be blocked");
    }

    let barrier = Arc::new(Barrier::new(9));
    let (result_tx, result_rx) = mpsc::channel();
    let producers = (0..8)
        .map(|id| {
            let service = service.clone();
            let barrier = Arc::clone(&barrier);
            let result_tx = result_tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let executions = Arc::new(AtomicUsize::new(0));
                let task_executions = Arc::clone(&executions);
                let result = service.submit_tracked_callable(move || {
                    task_executions.fetch_add(1, Ordering::SeqCst);
                    Ok::<usize, io::Error>(id)
                });
                result_tx
                    .send((id, result, executions))
                    .expect("coordinator should receive submission result");
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    drop(result_tx);

    let submissions = (0..8)
        .map(|_| result_rx.recv_timeout(DEADLINE).expect("submission should finish"))
        .collect::<Vec<_>>();
    for producer in producers {
        producer.join().expect("producer should not panic");
    }
    // The blockers remain held until every producer has reported its result.
    let accepted = submissions.iter().filter(|(_, result, _)| result.is_ok()).count();
    let saturated = submissions
        .iter()
        .filter(|(_, result, _)| matches!(result, Err(SubmissionError::Saturated)))
        .count();
    assert_eq!(accepted, 4);
    assert_eq!(saturated, 4);
    assert_eq!(service.stats().running, 2);
    assert_eq!(service.stats().queued, 4);
    assert_eq!(service.stats().cancelling, 0);
    assert_eq!(service.stats().task_capacity, 6);

    for _ in 0..2 {
        release_tx.send(()).expect("blocker should be released");
    }
    service.shutdown();
    assert!(service.wait_termination_timeout(DEADLINE));
    let deadline = Instant::now() + DEADLINE;
    for blocker in blockers {
        wait_for_result_until(blocker, deadline).expect("blocker should complete");
    }
    let mut executions_by_id = HashMap::new();
    for (id, result, executions) in submissions {
        executions_by_id.insert(id, (result, executions));
    }
    assert_eq!(executions_by_id.len(), 8);
    for (id, (result, executions)) in executions_by_id {
        let expected = match result {
            Ok(handle) => {
                assert_eq!(
                    wait_for_result_until(handle, deadline).expect("accepted task should succeed"),
                    id
                );
                1
            }
            Err(SubmissionError::Saturated) => 0,
            Err(error) => panic!("unexpected admission error: {error:?}"),
        };
        assert_eq!(executions.load(Ordering::SeqCst), expected, "task ID {id}");
    }
}

#[test]
fn test_completion_cancellation_and_submission_keep_progress() {
    let service = RayonExecutorService::builder()
        .num_threads(2)
        .task_capacity(4)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let mut blockers = Vec::new();
    for _ in 0..2 {
        let started_tx = started_tx.clone();
        let release_rx = Arc::clone(&release_rx);
        blockers.push(
            service
                .submit_tracked(move || {
                    started_tx.send(()).expect("coordinator should receive start");
                    release_rx
                        .lock()
                        .expect("release lock should not be poisoned")
                        .recv_timeout(DEADLINE)
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    Ok::<(), io::Error>(())
                })
                .expect("blocker should be accepted"),
        );
    }
    for _ in 0..2 {
        started_rx.recv_timeout(DEADLINE).expect("blocker should start");
    }

    let cancelled_count = Arc::new(AtomicUsize::new(0));
    let cancelled_count_task = Arc::clone(&cancelled_count);
    let cancel_target = service
        .submit_tracked_callable(move || {
            cancelled_count_task.fetch_add(1, Ordering::SeqCst);
            Ok::<usize, io::Error>(10)
        })
        .expect("cancel candidate should be accepted");
    let second_count = Arc::new(AtomicUsize::new(0));
    let second_count_task = Arc::clone(&second_count);
    let second_queued = service
        .submit_tracked_callable(move || {
            second_count_task.fetch_add(1, Ordering::SeqCst);
            Ok::<usize, io::Error>(11)
        })
        .expect("second queued task should be accepted");

    let barrier = Arc::new(Barrier::new(4));
    let cancel_barrier = Arc::clone(&barrier);
    let (done_tx, done_rx) = mpsc::channel();
    let mut producers = Vec::new();
    for id in 0..2 {
        let service = service.clone();
        let barrier = Arc::clone(&barrier);
        let done_tx = done_tx.clone();
        let count = Arc::new(AtomicUsize::new(0));
        producers.push(std::thread::spawn(move || {
            barrier.wait();
            let task_count = Arc::clone(&count);
            let result = service.submit_tracked_callable(move || {
                task_count.fetch_add(1, Ordering::SeqCst);
                Ok::<usize, io::Error>(20 + id)
            });
            done_tx
                .send((id, result, count))
                .expect("producer result should be reported");
        }));
    }
    let (cancel_done_tx, cancel_done_rx) = mpsc::channel();
    let (cancel_task_tx, cancel_task_rx) = mpsc::channel();
    let cancel_thread = std::thread::spawn(move || {
        cancel_barrier.wait();
        let result = cancel_target.cancel();
        cancel_done_tx.send(result).expect("cancel result should be reported");
        let task_result = cancel_target.get();
        cancel_task_tx
            .send(task_result)
            .expect("cancelled task result should be reported");
    });
    let started_at = Instant::now();
    barrier.wait();
    for _ in 0..2 {
        release_tx.send(()).expect("blocker should be released");
    }
    drop(done_tx);
    let deadline = started_at + DEADLINE;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let cancel_result = cancel_done_rx.recv_timeout(remaining).expect("cancel should finish");
    let produced = (0..2)
        .map(|_| {
            done_rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("producer should finish")
        })
        .collect::<Vec<_>>();
    for producer in producers {
        producer.join().expect("producer should not panic");
    }
    for blocker in blockers {
        wait_for_result_until(blocker, deadline).expect("blocker should finish");
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let cancel_task_result = cancel_task_rx
        .recv_timeout(remaining)
        .expect("cancelled task result should finish");
    let _ = cancel_thread;

    match cancel_result {
        CancelResult::Cancelled => {
            assert!(matches!(cancel_task_result, Err(TaskExecutionError::Cancelled)));
            assert_eq!(cancelled_count.load(Ordering::SeqCst), 0);
        }
        CancelResult::AlreadyRunning | CancelResult::AlreadyFinished => {
            assert_eq!(cancel_task_result.expect("running task should succeed"), 10);
            assert_eq!(cancelled_count.load(Ordering::SeqCst), 1);
        }
        other => panic!("unexpected cancel result: {other:?}"),
    }
    let second_result = wait_for_result_until(second_queued, deadline);
    assert!(second_result.is_ok());
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    let mut accepted_handles = Vec::new();
    let mut accepted_counts = HashMap::new();
    for (id, result, count) in produced {
        match result {
            Ok(handle) => {
                accepted_counts.insert(id, count);
                accepted_handles.push((id, handle));
            }
            Err(SubmissionError::Saturated) => {
                // The callable was consumed; the producer's unique ID is rebuilt below.
                let task_count = Arc::clone(&count);
                let handle = service
                    .submit_tracked_callable(move || {
                        task_count.fetch_add(1, Ordering::SeqCst);
                        Ok::<usize, io::Error>(20 + id)
                    })
                    .expect("capacity released by completion/cancellation should admit retry");
                accepted_counts.insert(id, count);
                accepted_handles.push((id, handle));
            }
            Err(error) => panic!("unexpected submission error: {error:?}"),
        }
    }
    for (id, handle) in accepted_handles {
        assert_eq!(
            wait_for_result_until(handle, deadline).expect("accepted task should finish"),
            20 + id
        );
        assert_eq!(accepted_counts[&id].load(Ordering::SeqCst), 1);
    }
    let follow_up_count = Arc::new(AtomicUsize::new(0));
    let task_count = Arc::clone(&follow_up_count);
    let follow_up = service
        .submit_tracked(move || {
            task_count.fetch_add(1, Ordering::SeqCst);
            Ok::<(), io::Error>(())
        })
        .expect("follow-up task should be admitted after concurrent work");
    wait_for_result_until(follow_up, deadline).expect("follow-up task should complete");
    assert_eq!(follow_up_count.load(Ordering::SeqCst), 1);
    service.shutdown();
    assert!(service.wait_termination_timeout(DEADLINE));
    let stats = service.stats();
    assert_eq!((stats.queued, stats.running, stats.cancelling), (0, 0, 0));
}
