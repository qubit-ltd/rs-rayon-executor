// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
// =============================================================================
mod common;

use std::io;
use std::sync::mpsc;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

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
            release_rx.recv().expect("running task should be released");
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    started_rx.recv().expect("running task should start");
    let queued = service
        .submit_tracked(|| Ok::<(), io::Error>(()))
        .expect("queued task should be accepted");
    assert!(matches!(
        service.submit_callable(|| Ok::<(), io::Error>(())),
        Err(SubmissionError::Saturated)
    ));
    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    let replacement = service
        .submit_callable(|| Ok::<(), io::Error>(()))
        .expect("cancelled capacity should be reusable");
    service.shutdown();
    assert!(matches!(
        service.submit_callable(|| Ok::<(), io::Error>(())),
        Err(SubmissionError::Shutdown)
    ));
    release_tx.send(()).expect("running task should be released");
    running.get().expect("running task should finish");
    replacement.get().expect("replacement task should finish");
    service.wait_termination();
}
