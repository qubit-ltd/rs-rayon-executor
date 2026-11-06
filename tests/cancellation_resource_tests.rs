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
use std::sync::mpsc;
use std::time::Duration;

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

struct ControlledDropProbe {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    drops: Arc<AtomicUsize>,
    panic_after_drop: bool,
}

impl Drop for ControlledDropProbe {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        self.release
            .recv_timeout(Duration::from_secs(10))
            .expect("test should release capture cleanup within timeout");
        self.drops.fetch_add(1, Ordering::SeqCst);
        if self.panic_after_drop {
            panic!("intentional capture destructor panic");
        }
    }
}

struct ReleaseGuard(Option<mpsc::Sender<()>>);

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

fn controlled_probe(
    drops: &Arc<AtomicUsize>,
    panic_after_drop: bool,
) -> (ControlledDropProbe, mpsc::Receiver<()>, ReleaseGuard) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    (
        ControlledDropProbe {
            entered: entered_tx,
            release: release_rx,
            drops: Arc::clone(drops),
            panic_after_drop,
        },
        entered_rx,
        ReleaseGuard(Some(release_tx)),
    )
}

fn wait_for_probe(receiver: mpsc::Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("capture cleanup should start within timeout");
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
    release_tx.send(()).expect("running task should release");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    running.get().expect("running task should finish");
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
    release_tx.send(()).expect("running task should release");
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    running.get().expect("running task should finish");
}

#[test]
fn test_cancel_keeps_capacity_until_capture_cleanup_finishes() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, running_release_tx) = common::helpers::submit_blocking_task(&service);
    let mut running_release = ReleaseGuard(Some(running_release_tx));
    let drops = Arc::new(AtomicUsize::new(0));
    let (probe, entered_rx, mut cleanup_release) = controlled_probe(&drops, false);
    let queued = service
        .submit_tracked_callable({
            let mut probe = Some(probe);
            move || {
                let _probe = probe.take();
                Ok::<(), io::Error>(())
            }
        })
        .expect("queued task should be accepted");
    #[cfg(feature = "async-wait")]
    let capacity_changes = service.capacity_changes();

    let (cancel_done_tx, cancel_done_rx) = mpsc::channel();
    let cancel_thread = std::thread::spawn(move || {
        let result = queued.cancel();
        let _ = cancel_done_tx.send(result);
    });
    wait_for_probe(entered_rx);
    assert_eq!(service.stats().queued, 0);
    assert_eq!(service.stats().running, 1);
    assert_eq!(service.stats().cancelling, 1);
    assert!(matches!(
        service.submit_callable(|| Ok::<(), io::Error>(())),
        Err(qubit_executor::service::SubmissionError::Saturated)
    ));

    cleanup_release.release();
    assert_eq!(
        cancel_done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("cancel should finish within timeout"),
        CancelResult::Cancelled
    );
    cancel_thread.join().expect("cancel thread should not panic");
    #[cfg(feature = "async-wait")]
    common::helpers::create_runtime().block_on(async {
        let mut capacity_changes = capacity_changes;
        tokio::time::timeout(Duration::from_secs(10), capacity_changes.changed())
            .await
            .expect("capacity change should be observable within timeout")
            .expect("capacity notification channel should remain open");
    });
    assert_eq!(service.stats().cancelling, 0);
    service
        .submit_callable(|| Ok::<(), io::Error>(()))
        .expect("capacity should be reusable after cleanup");

    running_release.release();
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    running.get().expect("running task should finish");
}

#[test]
fn test_stop_waits_for_capture_cleanup() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, running_release_tx) = common::helpers::submit_blocking_task(&service);
    let mut running_release = ReleaseGuard(Some(running_release_tx));
    let drops = Arc::new(AtomicUsize::new(0));
    let (probe, entered_rx, mut cleanup_release) = controlled_probe(&drops, false);
    service
        .submit_callable({
            let mut probe = Some(probe);
            move || {
                let _probe = probe.take();
                Ok::<(), io::Error>(())
            }
        })
        .expect("queued task should be accepted");
    let service_for_stop = service.clone();
    let (stop_done_tx, stop_done_rx) = mpsc::channel();
    let stop_thread = std::thread::spawn(move || {
        let result = service_for_stop.stop();
        let _ = stop_done_tx.send(result);
    });
    wait_for_probe(entered_rx);
    assert_eq!(service.stats().cancelling, 1);

    running_release.release();
    common::helpers::wait_until(|| service.stats().running == 0);
    assert_eq!(service.stats().cancelling, 1);
    assert!(!service.is_terminated());
    assert!(!service.wait_termination_timeout(Duration::from_millis(20)));
    #[cfg(feature = "async-wait")]
    {
        let runtime = common::helpers::create_runtime();
        assert!(
            runtime
                .block_on(async { tokio::time::timeout(Duration::from_millis(20), service.await_termination()).await })
                .is_err()
        );
    }

    cleanup_release.release();
    stop_done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("stop should finish after capture cleanup");
    stop_thread.join().expect("stop thread should finish");
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    running.get().expect("running task should finish");
    assert!(service.is_terminated());
    assert_eq!(service.stats().cancelling, 0);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    #[cfg(feature = "async-wait")]
    {
        let runtime = common::helpers::create_runtime();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), service.await_termination())
                .await
                .expect("terminated service should complete async wait");
        });
    }
}

#[test]
fn test_cancel_recovers_after_capture_destructor_panics() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, running_release_tx) = common::helpers::submit_blocking_task(&service);
    let mut running_release = ReleaseGuard(Some(running_release_tx));
    let drops = Arc::new(AtomicUsize::new(0));
    let (probe, entered_rx, mut cleanup_release) = controlled_probe(&drops, true);
    let queued = service
        .submit_tracked_callable({
            let mut probe = Some(probe);
            move || {
                let _probe = probe.take();
                Ok::<(), io::Error>(())
            }
        })
        .expect("queued task should be accepted");
    let (cancel_done_tx, cancel_done_rx) = mpsc::channel();
    let cancel_thread = std::thread::spawn(move || {
        let result = queued.cancel();
        let _ = cancel_done_tx.send(result);
    });
    wait_for_probe(entered_rx);
    cleanup_release.release();
    assert_eq!(
        cancel_done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("cancel should finish within timeout"),
        CancelResult::Cancelled
    );
    cancel_thread.join().expect("cancel thread should not panic");
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(service.stats().cancelling, 0);
    running_release.release();
    let follow_up = service
        .submit_tracked_callable(|| Ok::<(), io::Error>(()))
        .expect("service should accept a task after cleanup panic");
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    running.get().expect("running task should finish");
    follow_up.get().expect("follow-up task should execute");
}

#[test]
fn test_stop_continues_cleanup_after_capture_destructor_panics() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(3)
        .build()
        .expect("service should build");
    let (running, running_release_tx) = common::helpers::submit_blocking_task(&service);
    let mut running_release = ReleaseGuard(Some(running_release_tx));
    let drops = Arc::new(AtomicUsize::new(0));
    let (probe, entered_rx, mut cleanup_release) = controlled_probe(&drops, true);
    service
        .submit_callable({
            let mut probe = Some(probe);
            move || {
                let _probe = probe.take();
                Ok::<(), io::Error>(())
            }
        })
        .expect("panicking capture task should be accepted");
    let normal_drops = Arc::new(AtomicUsize::new(0));
    service
        .submit_callable({
            struct CountDrop(Arc<AtomicUsize>);
            impl Drop for CountDrop {
                fn drop(&mut self) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
            let normal_drop = CountDrop(Arc::clone(&normal_drops));
            move || {
                let _ = &normal_drop;
                Ok::<(), io::Error>(())
            }
        })
        .expect("normal queued task should be accepted");
    let service_for_stop = service.clone();
    let (stop_done_tx, stop_done_rx) = mpsc::channel();
    let stop_thread = std::thread::spawn(move || {
        let report = service_for_stop.stop();
        let _ = stop_done_tx.send(report);
    });
    wait_for_probe(entered_rx);
    cleanup_release.release();
    let report = stop_done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("stop should finish after panicking capture cleanup");
    stop_thread.join().expect("stop thread should finish");
    assert_eq!(report.queued, 2);
    assert_eq!(report.cancelled, 2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(normal_drops.load(Ordering::SeqCst), 1);
    assert_eq!(service.stats().cancelling, 0);
    running_release.release();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    running.get().expect("running task should finish");
    assert!(service.is_terminated());
}
