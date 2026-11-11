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

#[test]
fn test_rayon_executor_service_state_wait_termination_timeout_waits_for_last_task() {
    let service = create_single_worker_service();
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let running = service
        .submit_callable(move || {
            started_tx.send(()).expect("test should receive task start signal");
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<usize, io::Error>(42)
        })
        .expect("blocking task should be accepted");
    wait_started(started_rx);
    service.shutdown();
    assert!(!service.wait_termination_timeout(Duration::from_millis(20)));
    release_tx.send(()).expect("running task should receive release signal");
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(running.get().expect("running task should complete successfully"), 42);
    assert!(service.is_terminated());
}
