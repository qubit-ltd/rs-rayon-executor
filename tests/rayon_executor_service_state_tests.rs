// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! State-transition tests for
//! [`qubit_rayon_executor::RayonExecutorService`].

mod common;

use std::io;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

use crate::common::helpers::create_single_worker_service;
use crate::common::helpers::ok_usize_task;
use crate::common::helpers::submit_blocking_task;
use crate::common::helpers::wait_started;

#[test]
fn test_rayon_executor_service_state_stop_reports_running_and_queued() {
    let service = create_single_worker_service();
    let (running, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    let report = service.stop();

    assert_eq!(report.running, 1);
    assert_eq!(report.queued, 1);
    assert_eq!(report.cancelled, 1);
    release_tx.send(()).expect("running task should receive release signal");
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    running.get().expect("running task should complete after release");
    assert!(service.is_terminated());
}

#[tokio::test]
async fn test_rayon_executor_service_state_notifies_after_last_task_completes() {
    let service = RayonExecutorService::new().expect("service should be created");
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();

    let handle = service
        .submit_callable(move || {
            started_tx.send(()).expect("test should receive task start signal");
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<usize, io::Error>(42)
        })
        .expect("task should be accepted");
    wait_started(started_rx);
    let waiter_service = service.clone();
    let waiter = tokio::task::spawn_blocking(move || {
        assert!(waiter_service.wait_termination_timeout(Duration::from_secs(10)));
    });

    service.shutdown();
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "termination should wait while the accepted task is active",
    );

    release_tx.send(()).expect("running task should receive release signal");
    tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("termination waiter should finish within timeout")
        .expect("termination waiter should not panic");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("task result should become available within timeout")
            .expect("task should complete after release"),
        42
    );
    assert!(service.is_terminated());
}
