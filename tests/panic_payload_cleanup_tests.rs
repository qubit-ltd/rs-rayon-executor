// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

const DEADLINE: Duration = Duration::from_secs(10);

struct Capture(Arc<AtomicUsize>);

impl Drop for Capture {
    fn drop(&mut self) {
        std::panic::panic_any(Payload(Arc::clone(&self.0)));
    }
}

struct Payload(Arc<AtomicUsize>);

impl Drop for Payload {
    fn drop(&mut self) {
        std::panic::panic_any(Secondary(Arc::clone(&self.0)));
    }
}

struct Secondary(Arc<AtomicUsize>);

impl Drop for Secondary {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("secondary panic payload must be forgotten");
    }
}

struct ReleaseGuard(Option<std::sync::mpsc::Sender<()>>);

impl ReleaseGuard {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn panic_payload_task(drops: &Arc<AtomicUsize>) -> impl FnMut() -> Result<(), io::Error> + Send + 'static {
    let mut capture = Some(Capture(Arc::clone(drops)));
    move || {
        let _capture = capture.take();
        Ok(())
    }
}

fn assert_operation_does_not_unwind<T>(operation: impl FnOnce() -> T, name: &str) -> T {
    match std::panic::catch_unwind(AssertUnwindSafe(operation)) {
        Ok(value) => value,
        Err(payload) => {
            std::mem::forget(payload);
            panic!("{name} must contain panic payload cleanup");
        }
    }
}

#[test]
fn test_cancel_contains_panicking_panic_payload_cleanup() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, release_tx) = common::helpers::submit_blocking_task(&service);
    let mut release = ReleaseGuard(Some(release_tx));
    let secondary_drops = Arc::new(AtomicUsize::new(0));
    let queued = service
        .submit_tracked_callable(panic_payload_task(&secondary_drops))
        .expect("panic payload task should be accepted");

    let cancelled = assert_operation_does_not_unwind(|| queued.cancel(), "cancel");
    assert_eq!(cancelled, CancelResult::Cancelled);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    assert_eq!(service.stats().cancelling, 0);
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);

    let ordinary_runs = Arc::new(AtomicUsize::new(0));
    let ordinary_count = Arc::clone(&ordinary_runs);
    let ordinary = service
        .submit_tracked_callable(move || {
            ordinary_count.fetch_add(1, Ordering::SeqCst);
            Ok::<(), io::Error>(())
        })
        .expect("capacity should be reusable after cancellation");
    release.release();
    service.shutdown();
    assert!(service.wait_termination_timeout(DEADLINE));
    running.get().expect("blocker should finish");
    ordinary.get().expect("ordinary task should finish");
    assert_eq!(ordinary_runs.load(Ordering::SeqCst), 1);
    assert_eq!(service.stats().cancelling, 0);
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
}

#[test]
fn test_stop_contains_panicking_panic_payload_cleanup_and_cancels_later_tasks() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(3)
        .build()
        .expect("service should build");
    let (running, release_tx) = common::helpers::submit_blocking_task(&service);
    let mut release = ReleaseGuard(Some(release_tx));
    let secondary_drops = Arc::new(AtomicUsize::new(0));
    let panic_task = service
        .submit_tracked_callable(panic_payload_task(&secondary_drops))
        .expect("panic payload task should be accepted");
    let ordinary_runs = Arc::new(AtomicUsize::new(0));
    let ordinary_count = Arc::clone(&ordinary_runs);
    let ordinary = service
        .submit_tracked_callable(move || {
            ordinary_count.fetch_add(1, Ordering::SeqCst);
            Ok::<(), io::Error>(())
        })
        .expect("ordinary task should be accepted");

    let report = assert_operation_does_not_unwind(|| service.stop(), "stop");
    assert_eq!(report.queued, 2);
    assert_eq!(report.cancelled, 2);
    assert_eq!(ordinary_runs.load(Ordering::SeqCst), 0);
    assert!(matches!(panic_task.get(), Err(TaskExecutionError::Cancelled)));
    assert!(matches!(ordinary.get(), Err(TaskExecutionError::Cancelled)));
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);

    release.release();
    service.shutdown();
    assert!(service.wait_termination_timeout(DEADLINE));
    running.get().expect("blocker should finish");
    assert_eq!(ordinary_runs.load(Ordering::SeqCst), 0);
    assert_eq!(service.stats().queued, 0);
    assert_eq!(service.stats().running, 0);
    assert_eq!(service.stats().cancelling, 0);
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
}
