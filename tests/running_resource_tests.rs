// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::error::Error;
use std::fmt;
use std::io;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::TaskExecutionError;
use qubit_executor::TaskHandle;
use qubit_executor::TaskResult;
use qubit_executor::TryGet;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

const DEADLINE: Duration = Duration::from_secs(10);
const CHILD_MARKER: &str = "QUBIT_RUNNING_RESOURCE_CHILD";

struct ReleaseGuard(Option<mpsc::Sender<()>>);

impl ReleaseGuard {
    fn new(sender: mpsc::Sender<()>) -> Self {
        Self(Some(sender))
    }

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

struct Secondary(Arc<AtomicUsize>);

impl Drop for Secondary {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("secondary panic payload must be forgotten");
    }
}

struct PanicPayload(Arc<AtomicUsize>);

impl Drop for PanicPayload {
    fn drop(&mut self) {
        std::panic::panic_any(Secondary(Arc::clone(&self.0)));
    }
}

#[derive(Debug)]
struct PanicOnDrop(Arc<AtomicUsize>);

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("intentional result or error destructor panic");
    }
}

#[derive(Debug)]
struct DropError {
    _drop_probe: PanicOnDrop,
}

impl fmt::Display for DropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("intentional error")
    }
}

impl Error for DropError {}

fn service() -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .expect("service should build")
}

fn wait_signal(receiver: &mpsc::Receiver<()>, message: &str) {
    receiver
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("{message} within the deadline"));
}

fn wait_for_handle<R: Send + 'static, E: Send + 'static>(mut handle: TaskHandle<R, E>) -> TaskResult<R, E> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match handle.try_get() {
            TryGet::Ready(result) => return result,
            TryGet::Pending(pending) if Instant::now() < deadline => {
                handle = pending;
                std::thread::sleep(Duration::from_millis(1));
            }
            TryGet::Pending(_) => panic!("task result exceeded the deadline"),
        }
    }
}

fn submit_successor(service: &RayonExecutorService, runs: Arc<AtomicUsize>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let runs = Arc::clone(&runs);
        let observed_runs = Arc::clone(&runs);
        match service.submit_callable(move || {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok::<(), io::Error>(())
        }) {
            Ok(handle) => {
                service.shutdown();
                assert!(service.wait_termination_timeout(DEADLINE));
                handle.get().expect("successor should finish");
                assert_eq!(observed_runs.load(Ordering::SeqCst), 1);
                let stats = service.stats();
                assert_eq!(stats.queued, 0);
                assert_eq!(stats.running, 0);
                assert_eq!(stats.cancelling, 0);
                return;
            }
            Err(SubmissionError::Saturated) if Instant::now() < deadline => {
                std::thread::yield_now();
            }
            Err(error) => panic!("successor submission failed before capacity returned: {error}"),
        }
    }
}

fn run_isolated(name: &str, body: fn()) {
    if std::env::var(CHILD_MARKER).as_deref() == Ok(name) {
        body();
        return;
    }

    let mut child = Command::new(std::env::current_exe().expect("test executable path"))
        .args(["--exact", name, "--nocapture"])
        .env(CHILD_MARKER, name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("isolated regression child should start");
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait().expect("child status should be available") {
            Some(status) => {
                if !status.success() {
                    let output = child.wait_with_output().expect("child output should be collected");
                    panic!(
                        "isolated case {name} failed with {status}:\n{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                return;
            }
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated case {name} exceeded the 10 second deadline");
            }
        }
    }
}

#[test]
fn test_running_panic_payload_preserves_progress() {
    run_isolated(
        "test_running_panic_payload_preserves_progress",
        running_panic_payload_preserves_progress,
    );
}

fn running_panic_payload_preserves_progress() {
    let service = service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ReleaseGuard::new(release_tx);
    let secondary_drops = Arc::new(AtomicUsize::new(0));
    let mut payload_drops = Some(Arc::clone(&secondary_drops));
    let handle = service
        .submit_callable(move || -> Result<(), io::Error> {
            started_tx.send(()).expect("parent should observe task start");
            release_rx.recv_timeout(DEADLINE).expect("test should release task");
            std::panic::panic_any(PanicPayload(payload_drops.take().expect("payload is used once")));
        })
        .expect("panic task should be accepted");
    wait_signal(&started_rx, "panic task should start");
    release.release();
    assert!(matches!(wait_for_handle(handle), Err(TaskExecutionError::Panicked)));
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
    submit_successor(&service, Arc::new(AtomicUsize::new(0)));
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
}

#[test]
fn test_detached_error_drop_preserves_progress() {
    run_isolated(
        "test_detached_error_drop_preserves_progress",
        detached_error_drop_preserves_progress,
    );
}

fn detached_error_drop_preserves_progress() {
    let service = service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ReleaseGuard::new(release_tx);
    let drops = Arc::new(AtomicUsize::new(0));
    let mut error_drops = Some(Arc::clone(&drops));
    service
        .submit(move || -> Result<(), DropError> {
            started_tx.send(()).expect("parent should observe task start");
            release_rx.recv_timeout(DEADLINE).expect("test should release task");
            Err(DropError {
                _drop_probe: PanicOnDrop(error_drops.take().expect("error is produced once")),
            })
        })
        .expect("detached task should be accepted");
    wait_signal(&started_rx, "detached task should start");
    release.release();
    submit_successor(&service, Arc::new(AtomicUsize::new(0)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn test_detached_panic_payload_cleanup_preserves_progress() {
    run_isolated(
        "test_detached_panic_payload_cleanup_preserves_progress",
        detached_panic_payload_cleanup_preserves_progress,
    );
}

fn detached_panic_payload_cleanup_preserves_progress() {
    let service = service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ReleaseGuard::new(release_tx);
    let secondary_drops = Arc::new(AtomicUsize::new(0));
    let mut payload_drops = Some(Arc::clone(&secondary_drops));
    service
        .submit(move || -> Result<(), io::Error> {
            started_tx.send(()).expect("parent should observe task start");
            release_rx.recv_timeout(DEADLINE).expect("test should release task");
            std::panic::panic_any(PanicPayload(payload_drops.take().expect("payload is used once")));
        })
        .expect("detached panic task should be accepted");
    wait_signal(&started_rx, "detached panic task should start");
    release.release();
    submit_successor(&service, Arc::new(AtomicUsize::new(0)));
    assert_eq!(secondary_drops.load(Ordering::SeqCst), 1);
}

#[test]
fn test_dropped_handle_success_drop_preserves_progress() {
    run_isolated(
        "test_dropped_handle_success_drop_preserves_progress",
        dropped_handle_success_drop_preserves_progress,
    );
}

fn dropped_handle_success_drop_preserves_progress() {
    let service = service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ReleaseGuard::new(release_tx);
    let drops = Arc::new(AtomicUsize::new(0));
    let mut result_drops = Some(Arc::clone(&drops));
    let handle = service
        .submit_callable(move || -> Result<PanicOnDrop, io::Error> {
            started_tx.send(()).expect("parent should observe task start");
            release_rx.recv_timeout(DEADLINE).expect("test should release task");
            Ok(PanicOnDrop(result_drops.take().expect("result is produced once")))
        })
        .expect("callable should be accepted");
    wait_signal(&started_rx, "callable should start");
    drop(handle);
    release.release();
    submit_successor(&service, Arc::new(AtomicUsize::new(0)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn test_dropped_handle_error_drop_preserves_progress() {
    run_isolated(
        "test_dropped_handle_error_drop_preserves_progress",
        dropped_handle_error_drop_preserves_progress,
    );
}

fn dropped_handle_error_drop_preserves_progress() {
    let service = service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ReleaseGuard::new(release_tx);
    let drops = Arc::new(AtomicUsize::new(0));
    let mut error_drops = Some(Arc::clone(&drops));
    let handle = service
        .submit_callable(move || -> Result<(), DropError> {
            started_tx.send(()).expect("parent should observe task start");
            release_rx.recv_timeout(DEADLINE).expect("test should release task");
            Err(DropError {
                _drop_probe: PanicOnDrop(error_drops.take().expect("error is produced once")),
            })
        })
        .expect("callable should be accepted");
    wait_signal(&started_rx, "callable should start");
    drop(handle);
    release.release();
    submit_successor(&service, Arc::new(AtomicUsize::new(0)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
