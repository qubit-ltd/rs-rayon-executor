// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Regression tests for shared service lifecycle behavior.

use std::io;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

fn single_worker_service() -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should be created")
}

#[test]
fn test_clone_shutdown_rejects_submissions_and_terminates_shared_service() {
    let service = single_worker_service();
    let shutdown_clone = service.clone();

    shutdown_clone.shutdown();

    assert!(matches!(
        service.submit_callable(|| Ok::<usize, io::Error>(42)),
        Err(SubmissionError::Shutdown)
    ));
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(service.lifecycle(), ExecutorServiceLifecycle::Terminated);
    assert!(shutdown_clone.is_terminated());
}

#[test]
fn test_clone_stop_rejects_submissions_and_terminates_shared_service() {
    let service = single_worker_service();
    let stop_clone = service.clone();

    let _report = stop_clone.stop();

    assert!(matches!(
        service.submit_callable(|| Ok::<usize, io::Error>(42)),
        Err(SubmissionError::Shutdown)
    ));
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(service.lifecycle(), ExecutorServiceLifecycle::Terminated);
    assert!(stop_clone.is_terminated());
}

#[test]
fn test_dropping_clone_keeps_shared_service_admitting_and_completing_work() {
    let service = single_worker_service();
    let dropped_clone = service.clone();
    drop(dropped_clone);

    let handle = service
        .submit_callable(|| Ok::<usize, io::Error>(42))
        .expect("remaining clone should still accept work");

    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(handle.get().expect("accepted task should complete"), 42);
    assert_eq!(service.lifecycle(), ExecutorServiceLifecycle::Terminated);
}
