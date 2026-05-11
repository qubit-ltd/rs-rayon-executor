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
        atomic::{
            AtomicBool,
            Ordering,
        },
        mpsc,
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
    wait_started,
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
fn test_rayon_executor_service_stop_cancels_queued_tasks() {
    let service = create_single_worker_service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = service
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should be accepted");
    wait_started(started_rx);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    let report = service.stop();

    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Dropped)));
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
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = service
        .submit_tracked(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should be accepted");
    wait_started(started_rx);
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
        assert!(matches!(queued.get(), Err(TaskExecutionError::Dropped)));
    }
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
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
