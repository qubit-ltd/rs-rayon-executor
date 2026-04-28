/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Tests for [`RayonExecutorService`](qubit_rayon_executor::service::RayonExecutorService).

mod common;

use std::{
    io,
    sync::mpsc,
};

use qubit_executor::TaskExecutionError;
use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
};

use qubit_rayon_executor::RayonExecutorService;

use crate::common::{
    create_runtime,
    create_single_worker_service,
    ok_unit_task,
    ok_usize_task,
    wait_started,
};

#[test]
fn test_rayon_executor_service_submit_acceptance_is_not_task_success() {
    let service = RayonExecutorService::new().expect("service should be created");

    service
        .submit(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("service should accept shared runnable")
        .get()
        .expect("shared runnable should complete successfully");

    let handle = service
        .submit(|| Err::<(), _>(io::Error::other("task failed")))
        .expect("service should accept runnable");

    let err = handle
        .get()
        .expect_err("accepted runnable should report task failure through handle");
    assert!(matches!(err, TaskExecutionError::Failed(_)));
    service.shutdown();
    create_runtime().block_on(service.await_termination());
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
    create_runtime().block_on(service.await_termination());
}

#[test]
fn test_rayon_executor_service_shutdown_rejects_new_tasks() {
    let service = RayonExecutorService::new().expect("service should be created");

    service.shutdown();
    let result = service.submit(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(result, Err(RejectedExecution::Shutdown)));
    create_runtime().block_on(service.await_termination());
    assert!(service.is_shutdown());
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_shutdown_now_cancels_queued_tasks() {
    let service = create_single_worker_service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = service
        .submit(move || {
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

    let report = service.shutdown_now();

    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    create_runtime().block_on(service.await_termination());
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_shutdown_now_reports_all_queued_tasks() {
    let service = create_single_worker_service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = service
        .submit(move || {
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

    let report = service.shutdown_now();

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
    create_runtime().block_on(service.await_termination());
    assert!(service.is_terminated());
}

#[tokio::test]
async fn test_rayon_executor_service_await_termination_waits_before_shutdown() {
    let service = RayonExecutorService::new().expect("service should be created");
    let waiter_service = service.clone();
    let waiter = tokio::spawn(async move {
        waiter_service.await_termination().await;
    });

    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    service.shutdown();
    waiter
        .await
        .expect("termination waiter should finish after shutdown");
}
