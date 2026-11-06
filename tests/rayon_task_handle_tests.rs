// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Tests for [`qubit_rayon_executor::RayonTaskHandle].

mod common;

use std::future::IntoFuture;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::CancelResult;
use qubit_executor::TaskCancellationHandle;
use qubit_executor::TaskExecutionError;
use qubit_executor::TaskStatus;
use qubit_executor::TryGet;
use qubit_executor::service::ExecutorService;
use qubit_executor::task::spi::TaskResultHandle;
use qubit_executor::task::spi::TrackedTaskHandle;
use qubit_rayon_executor::RayonExecutorService;

use crate::common::helpers::create_single_worker_service;
use crate::common::helpers::ok_usize_task;
use crate::common::helpers::submit_blocking_task;
use crate::common::helpers::wait_until;

#[test]
fn test_rayon_cancellation_handle_cancels_after_handle_into_future() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");
    let cancellation: TaskCancellationHandle = queued.cancellation_handle();
    let result_future = queued.into_future();

    assert_eq!(cancellation.cancel(), CancelResult::Cancelled);
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime should build")
        .block_on(result_future);
    assert!(matches!(result, Err(TaskExecutionError::Cancelled)));

    release_tx.send(()).expect("running task should be released");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    first.get().expect("running task should complete");
}

#[test]
fn test_rayon_cancellation_handle_clone_and_drop_behavior() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");
    let endpoint = queued.cancellation_handle();
    drop(endpoint.clone());
    drop(endpoint);
    assert_eq!(queued.status(), TaskStatus::Pending);

    let endpoint = queued.cancellation_handle();
    assert_eq!(endpoint.clone().cancel(), CancelResult::Cancelled);
    assert_eq!(endpoint.cancel(), CancelResult::AlreadyFinished);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));

    release_tx.send(()).expect("running task should be released");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    first.get().expect("running task should complete");
}

#[test]
fn test_rayon_cancellation_handle_cannot_cancel_running_task() {
    let service = create_single_worker_service();
    let (running, release_tx) = submit_blocking_task(&service);
    let endpoint = running.cancellation_handle();
    assert_eq!(endpoint.cancel(), CancelResult::AlreadyRunning);
    release_tx.send(()).expect("running task should be released");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    running.get().expect("running task should complete");
}

#[test]
fn test_rayon_cancellation_handle_clones_can_cancel_concurrently() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");
    let endpoint = queued.cancellation_handle();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let cancel_threads: Vec<_> = (0..2)
        .map(|_| {
            let endpoint = endpoint.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                endpoint.cancel()
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = cancel_threads
        .into_iter()
        .map(|thread| thread.join().expect("cancel thread should not panic"))
        .collect();
    assert!(results.contains(&CancelResult::Cancelled));
    assert!(
        results
            .iter()
            .all(|result| matches!(result, CancelResult::Cancelled | CancelResult::AlreadyFinished))
    );
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));

    release_tx.send(()).expect("running task should be released");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    first.get().expect("running task should complete");
}

#[tokio::test]
async fn test_rayon_task_handle_can_be_awaited() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("handle should resolve within timeout")
            .expect("handle should await result"),
        42
    );
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
}

#[test]
fn test_rayon_task_handle_cancel_before_start_reports_cancelled() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(queued.is_done());
    service.shutdown();
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    first.get().expect("running task should complete normally");
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
}

#[test]
fn test_rayon_task_handle_reports_panicked_task() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_tracked(|| -> Result<(), io::Error> { panic!("rayon service panic") })
        .expect("service should accept panicking task");

    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert!(matches!(handle.get(), Err(TaskExecutionError::Panicked)));
}

#[test]
fn test_rayon_task_handle_cancel_after_completion_returns_false() {
    let service = create_single_worker_service();
    let handle = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    wait_until(|| handle.is_done());
    assert_eq!(handle.cancel(), CancelResult::AlreadyFinished);
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(handle.get().expect("callable should complete"), 42);
}

#[test]
fn test_rayon_task_handle_cancel_running_reports_already_running() {
    let service = create_single_worker_service();
    let (handle, release_tx) = submit_blocking_task(&service);

    assert_eq!(
        <_ as TrackedTaskHandle<(), io::Error>>::status(&handle),
        TaskStatus::Running,
    );
    assert_eq!(
        <_ as TrackedTaskHandle<(), io::Error>>::cancel(&handle),
        CancelResult::AlreadyRunning,
    );
    assert!(!<_ as TaskResultHandle<(), io::Error>>::is_done(&handle));

    service.shutdown();
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    <_ as TaskResultHandle<(), io::Error>>::get(handle).expect("task should complete");
}

#[test]
fn test_rayon_task_handle_cancel_during_start_race_never_reports_unsupported() {
    const QUEUED_TASKS: usize = 64;
    const ATTEMPTS: usize = 64;

    for _ in 0..ATTEMPTS {
        let service = create_single_worker_service();
        let (running, release_tx) = submit_blocking_task(&service);

        let queued_handles = (0..QUEUED_TASKS)
            .map(|_| {
                let executions = Arc::new(AtomicUsize::new(0));
                let task_executions = Arc::clone(&executions);
                let handle = service
                    .submit_tracked(move || {
                        task_executions.fetch_add(1, Ordering::SeqCst);
                        Ok::<(), io::Error>(())
                    })
                    .expect("queued task should be accepted");
                (handle, executions)
            })
            .collect::<Vec<_>>();

        release_tx.send(()).expect("running task should receive release signal");
        let cancel_results = queued_handles
            .iter()
            .map(|(handle, _)| {
                let result = handle.cancel();
                assert_ne!(
                    result,
                    CancelResult::Unsupported,
                    "rayon task handles should keep cancellation semantics while a task is starting",
                );
                result
            })
            .collect::<Vec<_>>();

        service.shutdown();
        assert!(service.wait_termination_timeout(Duration::from_secs(10)));
        running.get().expect("running task should complete normally");
        for ((handle, executions), cancel_result) in queued_handles.into_iter().zip(cancel_results) {
            let result = handle.get();
            match cancel_result {
                CancelResult::Cancelled => {
                    assert!(matches!(result, Err(TaskExecutionError::Cancelled)));
                    assert_eq!(executions.load(Ordering::SeqCst), 0);
                }
                CancelResult::AlreadyRunning | CancelResult::AlreadyFinished => {
                    assert!(result.is_ok());
                    assert_eq!(executions.load(Ordering::SeqCst), 1);
                }
                other => panic!("unexpected cancellation result: {other:?}"),
            }
        }
        let stats = service.stats();
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.running, 0);
        assert_eq!(stats.cancelling, 0);
    }
}

#[test]
fn test_rayon_cancellation_endpoint_during_start_race_matches_task_result() {
    const QUEUED_TASKS: usize = 64;
    const ATTEMPTS: usize = 64;

    for _ in 0..ATTEMPTS {
        let service = create_single_worker_service();
        let (running, release_tx) = submit_blocking_task(&service);

        let queued_tasks = (0..QUEUED_TASKS)
            .map(|_| {
                let executions = Arc::new(AtomicUsize::new(0));
                let task_executions = Arc::clone(&executions);
                let handle = service
                    .submit_tracked(move || {
                        task_executions.fetch_add(1, Ordering::SeqCst);
                        Ok::<(), io::Error>(())
                    })
                    .expect("queued task should be accepted");
                let cancellation = handle.cancellation_handle();
                (cancellation, handle.into_future(), executions)
            })
            .collect::<Vec<_>>();

        release_tx.send(()).expect("running task should receive release signal");
        let cancel_results = queued_tasks
            .iter()
            .map(|(cancellation, _, _)| cancellation.cancel())
            .collect::<Vec<_>>();

        service.shutdown();
        assert!(service.wait_termination_timeout(Duration::from_secs(10)));
        running.get().expect("running task should complete normally");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        for ((_, result_future, executions), cancel_result) in queued_tasks.into_iter().zip(cancel_results) {
            let result = runtime.block_on(result_future);
            match cancel_result {
                CancelResult::Cancelled => {
                    assert!(matches!(result, Err(TaskExecutionError::Cancelled)));
                    assert_eq!(executions.load(Ordering::SeqCst), 0);
                }
                CancelResult::AlreadyRunning | CancelResult::AlreadyFinished => {
                    assert!(result.is_ok());
                    assert_eq!(executions.load(Ordering::SeqCst), 1);
                }
                other => panic!("unexpected cancellation result: {other:?}"),
            }
        }
        let stats = service.stats();
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.running, 0);
        assert_eq!(stats.cancelling, 0);
    }
}

#[test]
fn test_rayon_task_handle_reports_status_and_try_get_states() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);

    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");
    assert_eq!(queued.status(), TaskStatus::Pending);

    let mut queued = match queued.try_get() {
        TryGet::Pending(queued) => queued,
        TryGet::Ready(result) => {
            panic!("queued task should not be ready yet: {result:?}")
        }
    };

    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    first.get().expect("running task should complete normally");
    wait_until(|| queued.status() == TaskStatus::Succeeded);
    assert_eq!(queued.status(), TaskStatus::Succeeded);

    let deadline = Instant::now() + Duration::from_secs(2);
    let result = loop {
        match queued.try_get() {
            TryGet::Ready(result) => break result,
            TryGet::Pending(pending) if Instant::now() < deadline => {
                queued = pending;
                thread::sleep(Duration::from_millis(10));
            }
            TryGet::Pending(_) => {
                panic!("completed task result should become ready")
            }
        }
    };

    assert_eq!(result.expect("callable should complete"), 42);
}
