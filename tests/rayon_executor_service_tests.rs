/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`RayonExecutorService`](qubit_rayon_executor::service::RayonExecutorService).

mod common;

use std::{
    io,
    sync::{
        Arc,
        Barrier,
        atomic::{
            AtomicBool,
            Ordering,
        },
    },
};

use qubit_executor::TaskExecutionError;
use qubit_executor::service::{
    ExecutorService,
    ExecutorServiceLifecycle,
    SubmissionError,
};

use qubit_rayon_executor::RayonExecutorService;

use crate::common::helpers::{
    create_single_worker_service,
    ok_unit_task,
    ok_usize_task,
    submit_blocking_task,
};

#[test]
fn test_rayon_executor_service_submit_acceptance_is_not_task_success() {
    let service = RayonExecutorService::new().expect("service should be created");

    service
        .submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("service should accept shared runnable")
        .get()
        .expect("shared runnable should complete successfully");

    let handle = service
        .submit_tracked(|| Err::<(), _>(io::Error::other("task failed")))
        .expect("service should accept runnable");

    let err = handle
        .get()
        .expect_err("accepted runnable should report task failure through handle");
    assert!(matches!(err, TaskExecutionError::Failed(_)));
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_rayon_executor_service_submit_callable_returns_value() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    assert_eq!(
        handle.get().expect("callable should complete successfully"),
        42,
    );
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_rayon_executor_service_submit_runs_detached_task() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should be created");
    let completed = Arc::new(AtomicBool::new(false));
    let completed_for_task = Arc::clone(&completed);

    service
        .submit(move || {
            completed_for_task.store(true, Ordering::Release);
            Ok::<(), io::Error>(())
        })
        .expect("service should accept runnable");

    service.shutdown();
    service.wait_termination();

    assert!(completed.load(Ordering::Acquire));
    assert_eq!(service.lifecycle(), ExecutorServiceLifecycle::Terminated);
    assert!(service.is_not_running());
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_shutdown_rejects_new_tasks() {
    let service = RayonExecutorService::new().expect("service should be created");

    service.shutdown();
    let result = service.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(result, Err(SubmissionError::Shutdown)));
    service.wait_termination();
    assert!(service.is_not_running());
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_shutdown_allows_queued_tasks_to_finish() {
    let service = create_single_worker_service();
    let (running, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    service.shutdown();
    let rejected = service.submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>);
    assert!(matches!(rejected, Err(SubmissionError::Shutdown)));

    release_tx
        .send(())
        .expect("running task should receive release signal");
    running
        .get()
        .expect("running task should complete normally");
    assert_eq!(
        queued.get().expect("queued task should complete normally"),
        42
    );
    service.wait_termination();
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_stop_rejects_new_tasks_and_is_idempotent_when_empty() {
    let service = RayonExecutorService::new().expect("service should be created");

    let report = service.stop();
    assert_eq!(report.queued, 0);
    assert_eq!(report.running, 0);
    assert_eq!(report.cancelled, 0);

    let rejected = service.submit_tracked(ok_unit_task as fn() -> Result<(), io::Error>);
    assert!(matches!(rejected, Err(SubmissionError::Shutdown)));

    let second_report = service.stop();
    assert_eq!(second_report.queued, 0);
    assert_eq!(second_report.running, 0);
    assert_eq!(second_report.cancelled, 0);
    service.wait_termination();
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_stop_cancels_queued_tasks() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    let report = service.stop();

    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    service.wait_termination();
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_stop_reports_all_queued_tasks() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued_handles = (0..3)
        .map(|_| {
            service
                .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
                .expect("queued task should be accepted")
        })
        .collect::<Vec<_>>();

    let report = service.stop();

    assert_eq!(report.queued, 3);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 3);
    for queued in queued_handles {
        assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    }
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    service.wait_termination();
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_stop_is_safe_when_called_concurrently() {
    const QUEUED_TASKS: usize = 8_192;

    let service = create_single_worker_service();
    let (running, release_tx) = submit_blocking_task(&service);
    let queued_handles = (0..QUEUED_TASKS)
        .map(|_| {
            service
                .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
                .expect("queued task should be accepted")
        })
        .collect::<Vec<_>>();

    let barrier = Arc::new(Barrier::new(3));
    let first_stop_service = service.clone();
    let first_stop_barrier = Arc::clone(&barrier);
    let first_stop = std::thread::spawn(move || {
        first_stop_barrier.wait();
        first_stop_service.stop()
    });
    let second_stop_service = service.clone();
    let second_stop_barrier = Arc::clone(&barrier);
    let second_stop = std::thread::spawn(move || {
        second_stop_barrier.wait();
        second_stop_service.stop()
    });

    barrier.wait();
    let reports = [
        first_stop.join().expect("first stop should not panic"),
        second_stop.join().expect("second stop should not panic"),
    ];

    assert!(
        reports
            .iter()
            .any(|report| report.queued == QUEUED_TASKS && report.cancelled == QUEUED_TASKS),
        "one concurrent stop should cancel all queued tasks: {reports:?}",
    );
    assert!(
        reports
            .iter()
            .any(|report| report.queued == 0 && report.cancelled == 0),
        "one concurrent stop should observe no remaining queued tasks: {reports:?}",
    );
    for queued in queued_handles {
        assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    }

    release_tx
        .send(())
        .expect("running task should receive release signal");
    running
        .get()
        .expect("running task should complete normally");
    service.wait_termination();
    assert!(service.is_terminated());
}

#[tokio::test]
async fn test_rayon_executor_service_await_termination_waits_before_shutdown() {
    let service = RayonExecutorService::new().expect("service should be created");
    let waiter_service = service.clone();
    let waiter = tokio::task::spawn_blocking(move || {
        waiter_service.wait_termination();
    });

    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    service.shutdown();
    waiter
        .await
        .expect("termination waiter should finish after shutdown");
}
