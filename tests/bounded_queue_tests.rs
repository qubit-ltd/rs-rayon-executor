// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::io;
use std::sync::mpsc;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

use crate::common::helpers::wait_started;

#[test]
fn test_bounded_queue_rejects_when_full_and_reuses_cancelled_capacity() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = service
        .submit_tracked(move || {
            started_tx.send(()).expect("running task should start");
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("running task should be released");
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    wait_started(started_rx);
    let queued = service
        .submit_tracked(|| Ok::<(), io::Error>(()))
        .expect("queued task should be accepted");
    assert!(matches!(
        service.submit_callable(|| Ok::<(), io::Error>(())),
        Err(SubmissionError::Saturated)
    ));
    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    let replacement = service
        .submit_callable(|| Ok::<(), io::Error>(()))
        .expect("cancelled capacity should be reusable");
    service.shutdown();
    assert!(matches!(
        service.submit_callable(|| Ok::<(), io::Error>(())),
        Err(SubmissionError::Shutdown)
    ));
    release_tx.send(()).expect("running task should be released");
    assert!(service.wait_termination_timeout(std::time::Duration::from_secs(10)));
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    running.get().expect("running task should finish");
    replacement.get().expect("replacement task should finish");
}
