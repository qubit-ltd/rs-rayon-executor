// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
#![cfg(feature = "async-wait")]

use std::future::Future;
use std::io;
use std::io::Read;
use std::pin::Pin;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::task::Context;
use std::task::Poll;
use std::task::Wake;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::CancelResult;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;
use qubit_rayon_executor::RayonTaskHandle;

const CHILD_CASE: &str = "RXE_NOTIFICATION_UNWIND_CASE";
const CHILD_DEADLINE: Duration = Duration::from_secs(10);
const SCENARIO_DEADLINE: Duration = Duration::from_secs(5);

type CapacityWaiter<'a> = Pin<Box<dyn Future<Output = Result<(), tokio::sync::watch::error::RecvError>> + Send + 'a>>;
type CapacityObserver<'a> = (CapacityWaiter<'a>, Arc<CountWake>);

struct PanicOnceWake {
    fired: AtomicBool,
}

struct CountWake(AtomicUsize);

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Wake for PanicOnceWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.fired.swap(true, Ordering::SeqCst) {
            panic!("intentional notification waker panic");
        }
    }
}

impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_completion_waker_panic_preserves_successor() {
    run_in_child("test_completion_waker_panic_preserves_successor", || {
        let service = single_worker_service(2);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = service
            .submit_tracked_callable(move || {
                started_tx.send(()).expect("coordinator should receive start");
                release_rx
                    .recv_timeout(SCENARIO_DEADLINE)
                    .map_err(|error| io::Error::other(error.to_string()))?;
                Ok::<(), io::Error>(())
            })
            .expect("blocker should be accepted");
        started_rx
            .recv_timeout(SCENARIO_DEADLINE)
            .expect("blocker should start");

        let (successor_tx, successor_rx) = mpsc::channel();
        service
            .submit(move || {
                successor_tx.send(()).expect("coordinator should receive successor");
                Ok::<(), io::Error>(())
            })
            .expect("successor should be accepted");

        let panic_wake = Arc::new(PanicOnceWake {
            fired: AtomicBool::new(false),
        });
        let panic_waker = std::task::Waker::from(panic_wake.clone());
        let mut panic_waiter = capacity_waiter(&service);
        assert!(poll(&mut panic_waiter, &panic_waker).is_pending());
        let normal_waiters = capacity_observers(&service, 8);

        release_tx.send(()).expect("blocker should be released");
        successor_rx
            .recv_timeout(SCENARIO_DEADLINE)
            .expect("successor should run after the panicking capacity waker");
        blocker.get().expect("blocker should complete");
        assert!(panic_wake.fired.load(Ordering::SeqCst));
        assert_observers_notified(normal_waiters);

        drop(panic_waiter);
        service.shutdown();
        assert!(service.wait_termination_timeout(CHILD_DEADLINE));
        assert_zero_occupancy(&service);
    });
}

#[test]
fn test_cancel_waker_panic_preserves_capacity() {
    run_in_child("test_cancel_waker_panic_preserves_capacity", || {
        let service = single_worker_service(3);
        let (blocker, release_tx) = running_blocker(&service);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut probe = Some(DropProbe(Arc::clone(&drops)));
        let queued = service
            .submit_tracked_callable(move || {
                drop(probe.take());
                Ok::<(), io::Error>(())
            })
            .expect("queued task should be accepted");
        let cancellation = queued.cancellation_handle();

        let panic_wake = Arc::new(PanicOnceWake {
            fired: AtomicBool::new(false),
        });
        let panic_waker = std::task::Waker::from(panic_wake.clone());
        let mut panic_waiter = capacity_waiter(&service);
        assert!(poll(&mut panic_waiter, &panic_waker).is_pending());
        let normal_waiters = capacity_observers(&service, 8);

        assert_eq!(cancellation.cancel(), CancelResult::Cancelled);
        assert!(panic_wake.fired.load(Ordering::SeqCst));
        assert_eq!(drops.load(Ordering::SeqCst), 1, "cancelled capture should drop once");
        assert_eq!(service.stats().cancelling, 0);
        service
            .submit(|| Ok::<(), io::Error>(()))
            .expect("capacity should be reusable after cancellation");
        assert_observers_notified(normal_waiters);

        drop(panic_waiter);
        release_tx.send(()).expect("blocker should be released");
        blocker.get().expect("blocker should complete");
        service.shutdown();
        assert!(service.wait_termination_timeout(CHILD_DEADLINE));
        assert_zero_occupancy(&service);
    });
}

#[test]
fn test_stop_waker_panic_finishes_queued_cleanup() {
    run_in_child("test_stop_waker_panic_finishes_queued_cleanup", || {
        let service = single_worker_service(3);
        let (blocker, release_tx) = running_blocker(&service);
        let drops = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let mut probe = Some(DropProbe(Arc::clone(&drops)));
            service
                .submit(move || {
                    drop(probe.take());
                    Ok::<(), io::Error>(())
                })
                .expect("queued task should be accepted");
        }

        let panic_wake = Arc::new(PanicOnceWake {
            fired: AtomicBool::new(false),
        });
        let panic_waker = std::task::Waker::from(panic_wake.clone());
        let mut panic_waiter = capacity_waiter(&service);
        assert!(poll(&mut panic_waiter, &panic_waker).is_pending());
        let normal_waiters = capacity_observers(&service, 8);

        let report = service.stop();
        assert_eq!(report.cancelled, 2);
        assert!(panic_wake.fired.load(Ordering::SeqCst));
        assert_eq!(drops.load(Ordering::SeqCst), 2, "each queued capture should drop once");
        assert_eq!(service.stats().cancelling, 0);
        assert!(matches!(
            service.submit(|| Ok::<(), io::Error>(())),
            Err(SubmissionError::Shutdown)
        ));
        assert_observers_notified(normal_waiters);

        drop(panic_waiter);
        release_tx.send(()).expect("blocker should be released");
        blocker.get().expect("blocker should complete");
        assert!(service.wait_termination_timeout(CHILD_DEADLINE));
        assert_zero_occupancy(&service);
    });
}

#[test]
fn test_shutdown_waker_panic_preserves_other_waiters() {
    run_in_child("test_shutdown_waker_panic_preserves_other_waiters", || {
        let service = single_worker_service(1);
        let panic_wake = Arc::new(PanicOnceWake {
            fired: AtomicBool::new(false),
        });
        let panic_waker = std::task::Waker::from(panic_wake.clone());
        let mut panic_capacity_waiter = capacity_waiter(&service);
        assert!(poll(&mut panic_capacity_waiter, &panic_waker).is_pending());
        let capacity_observers = capacity_observers(&service, 8);

        let mut panic_termination_waiter = Box::pin(service.await_termination());
        assert!(poll(&mut panic_termination_waiter, &panic_waker).is_pending());
        let termination_wakes = Arc::new(CountWake(AtomicUsize::new(0)));
        let termination_waker = std::task::Waker::from(termination_wakes.clone());
        let mut normal_termination_waiter = Box::pin(service.await_termination());
        assert!(poll(&mut normal_termination_waiter, &termination_waker).is_pending());

        service.shutdown();
        assert!(panic_wake.fired.load(Ordering::SeqCst));
        assert_observers_notified(capacity_observers);
        assert!(
            termination_wakes.0.load(Ordering::SeqCst) > 0,
            "termination waiter should wake"
        );
        assert!(poll(&mut panic_capacity_waiter, &panic_waker).is_ready());
        assert!(poll(&mut panic_termination_waiter, &panic_waker).is_ready());
        assert!(poll(&mut normal_termination_waiter, &termination_waker).is_ready());
        assert!(service.is_terminated());
        assert_zero_occupancy(&service);
    });
}

#[test]
fn test_capacity_changes_receiver_controls_and_channel_closure() {
    let service = single_worker_service(1);
    let mut changes = service.capacity_changes();
    let same_channel = service.capacity_changes();
    let other_service = single_worker_service(1);
    let other_channel = other_service.capacity_changes();

    assert!(changes.same_channel(&same_channel));
    assert!(!changes.same_channel(&other_channel));
    assert_eq!(*changes.borrow(), 0);
    assert!(!changes.has_changed().expect("receiver should be open"));

    changes.mark_changed();
    assert!(changes.has_changed().expect("receiver should be open"));
    assert_eq!(*changes.borrow_and_update(), 0);
    assert!(!changes.has_changed().expect("receiver should be open"));
    changes.mark_changed();
    changes.mark_unchanged();
    assert!(!changes.has_changed().expect("receiver should be open"));

    service.shutdown();
    assert!(changes.has_changed().expect("receiver should still be open"));
    assert_eq!(*changes.borrow_and_update(), 1);
    assert!(!changes.is_closed());

    drop(same_channel);
    drop(other_channel);
    drop(other_service);
    drop(service);
    assert!(changes.is_closed());
    assert!(changes.has_changed().is_err());
}

fn single_worker_service(task_capacity: usize) -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(task_capacity)
        .build()
        .expect("service should build")
}

fn running_blocker(service: &RayonExecutorService) -> (RayonTaskHandle<(), io::Error>, mpsc::Sender<()>) {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocker = service
        .submit_tracked_callable(move || {
            started_tx.send(()).expect("coordinator should receive start");
            release_rx
                .recv_timeout(SCENARIO_DEADLINE)
                .map_err(|error| io::Error::other(error.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("blocker should be accepted");
    started_rx
        .recv_timeout(SCENARIO_DEADLINE)
        .expect("blocker should start");
    (blocker, release_tx)
}

fn capacity_waiter(service: &RayonExecutorService) -> CapacityWaiter<'_> {
    let mut receiver = service.capacity_changes();
    Box::pin(async move { receiver.changed().await })
}

fn capacity_observers(service: &RayonExecutorService, count: usize) -> Vec<CapacityObserver<'_>> {
    (0..count)
        .map(|_| {
            let wake = Arc::new(CountWake(AtomicUsize::new(0)));
            let waker = std::task::Waker::from(wake.clone());
            let mut waiter = capacity_waiter(service);
            assert!(poll(&mut waiter, &waker).is_pending());
            (waiter, wake)
        })
        .collect()
}

fn assert_observers_notified(observers: Vec<CapacityObserver<'_>>) {
    for (mut waiter, wake) in observers {
        assert!(
            wake.0.load(Ordering::SeqCst) > 0,
            "normal observer should receive an actual wake"
        );
        assert!(poll(&mut waiter, &std::task::Waker::from(wake)).is_ready());
    }
}

fn assert_zero_occupancy(service: &RayonExecutorService) {
    let stats = service.stats();
    assert_eq!((stats.queued, stats.running, stats.cancelling), (0, 0, 0));
}

fn poll<F: Future + ?Sized>(future: &mut Pin<Box<F>>, waker: &std::task::Waker) -> Poll<F::Output> {
    let mut context = Context::from_waker(waker);
    future.as_mut().poll(&mut context)
}

fn run_in_child(test_name: &str, scenario: impl FnOnce()) {
    if std::env::var_os(CHILD_CASE).as_deref() == Some(std::ffi::OsStr::new(test_name)) {
        scenario();
        return;
    }

    let mut child = Command::new(std::env::current_exe().expect("test executable path"))
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_CASE, test_name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("child test should start");
    let mut stdout = child.stdout.take().expect("child stdout should be piped");
    let mut stderr = child.stderr.take().expect("child stderr should be piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .read_to_end(&mut output)
            .expect("child stdout should be readable");
        output
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr
            .read_to_end(&mut output)
            .expect("child stderr should be readable");
        output
    });

    let deadline = Instant::now() + CHILD_DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("timed out child should be killed");
            let status = child.wait().expect("killed child should be reaped");
            let stdout = stdout_reader.join().expect("stdout reader should finish");
            let stderr = stderr_reader.join().expect("stderr reader should finish");
            let stdout = String::from_utf8_lossy(&stdout);
            let stderr = String::from_utf8_lossy(&stderr);
            panic!("child test {test_name} timed out ({status})\n{stdout}\n{stderr}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader.join().expect("stdout reader should finish");
    let stderr = stderr_reader.join().expect("stderr reader should finish");
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    assert!(
        status.success(),
        "child test {test_name} failed ({status})\n{stdout}\n{stderr}"
    );
}
