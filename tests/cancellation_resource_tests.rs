// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_cancel_releases_queued_callable_capture_before_running_task_finishes() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, release_tx) = common::helpers::submit_blocking_task(&service);
    let drops = Arc::new(AtomicUsize::new(0));
    let queued = service
        .submit_tracked_callable({
            let mut probe = Some(DropProbe(Arc::clone(&drops)));
            move || {
                let _probe = probe.take();
                Ok::<(), io::Error>(())
            }
        })
        .expect("queued task should be accepted");
    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx.send(()).expect("running task should release");
    running.get().expect("running task should finish");
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_stop_releases_queued_callable_capture_before_running_task_finishes() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, release_tx) = common::helpers::submit_blocking_task(&service);
    let drops = Arc::new(AtomicUsize::new(0));
    let queued = service
        .submit_tracked_callable({
            let mut probe = Some(DropProbe(Arc::clone(&drops)));
            move || {
                let _probe = probe.take();
                Ok::<(), io::Error>(())
            }
        })
        .expect("queued task should be accepted");

    let report = service.stop();
    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));

    release_tx.send(()).expect("running task should release");
    running.get().expect("running task should finish");
    service.wait_termination();
}
