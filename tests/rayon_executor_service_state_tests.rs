/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! State-transition tests for [`RayonExecutorService`](qubit_rayon_executor::service::RayonExecutorService).

mod common;

use std::{
    io,
    sync::mpsc,
};

use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

use crate::common::helpers::{
    create_runtime,
    create_single_worker_service,
    ok_usize_task,
    wait_started,
};

#[test]
fn test_rayon_executor_service_state_shutdown_now_reports_running_and_queued() {
    let service = create_single_worker_service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let running = service
        .submit(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    let report = service.shutdown_now();

    assert_eq!(report.running, 1);
    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx
        .send(())
        .expect("running task should receive release signal");
    running
        .get()
        .expect("running task should complete after release");
    create_runtime().block_on(service.await_termination());
    assert!(service.is_terminated());
}

#[tokio::test]
async fn test_rayon_executor_service_state_notifies_after_last_task_completes() {
    let service = RayonExecutorService::new().expect("service should be created");
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();

    let handle = service
        .submit_callable(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<usize, io::Error>(42)
        })
        .expect("task should be accepted");
    wait_started(started_rx);
    let waiter_service = service.clone();
    let waiter = tokio::spawn(async move {
        waiter_service.await_termination().await;
    });

    service.shutdown();
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "termination should wait while the accepted task is active",
    );

    release_tx
        .send(())
        .expect("running task should receive release signal");
    assert_eq!(
        handle.await.expect("task should complete after release"),
        42
    );
    waiter
        .await
        .expect("termination waiter should finish after final task completes");
    assert!(service.is_terminated());
}
