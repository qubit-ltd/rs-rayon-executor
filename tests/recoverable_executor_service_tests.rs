// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Integration tests for the shared recoverable submission trait.
//!
//! Each contract scenario runs in a bounded child process so a stalled executor
//! cannot hang the test suite.
mod common;

use std::io;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::TaskHandle;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::RecoverableExecutorService;
use qubit_executor::service::RejectedTask;
use qubit_executor::service::SubmissionError;
use qubit_function::Callable;
use qubit_rayon_executor::RayonExecutorService;

const CHANNEL_DEADLINE: Duration = Duration::from_secs(10);
const CHILD_DEADLINE: Duration = Duration::from_secs(30);
const CHILD_SCENARIO: &str = "RRE_RECOVERABLE_EXECUTOR_SCENARIO";

/// Tracks callable execution and destruction while retaining a payload for
/// ownership checks.
struct Probe {
    payload: Box<usize>,
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl Callable<usize, io::Error> for Probe {
    fn call(&mut self) -> Result<usize, io::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(*self.payload)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn attempt<S, C>(service: &S, task: C) -> Result<S::ResultHandle<usize, io::Error>, RejectedTask<C>>
where
    S: RecoverableExecutorService,
    C: Callable<usize, io::Error> + Send + 'static,
{
    RecoverableExecutorService::try_submit_callable(service, task)
}

/// Runs one contract body in a bounded subprocess and verifies its completion
/// marker.
fn run_scenario(test_name: &str, scenario: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_SCENARIO).as_deref() == Ok(scenario) {
        body();
        println!("RRE_SCENARIO_COMPLETED:{scenario}");
        return;
    }

    let executable = std::env::current_exe().expect("current test executable should be available");
    let mut command = Command::new(executable);
    command
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_SCENARIO, scenario);
    let bounded =
        common::subprocess::run_bounded_output(&mut command, CHILD_DEADLINE).expect("scenario subprocess should run");
    let stdout = String::from_utf8_lossy(&bounded.output.stdout);
    let stderr = String::from_utf8_lossy(&bounded.output.stderr);
    assert!(
        !bounded.timed_out,
        "scenario subprocess timed out\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        bounded.output.status.success(),
        "scenario subprocess failed with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        bounded.output.status
    );
    assert!(
        stdout.contains(&format!("RRE_SCENARIO_COMPLETED:{scenario}")),
        "scenario completion marker missing\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Creates a single-worker service with a running task that holds its only
/// queue slot.
fn service_with_blocker() -> (RayonExecutorService, TaskHandle<(), io::Error>, mpsc::Sender<()>) {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocker = service
        .submit_callable(move || {
            started_tx.send(()).expect("coordinator should receive start");
            release_rx
                .recv_timeout(CHANNEL_DEADLINE)
                .map_err(|error| io::Error::other(error.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("blocker should be accepted");
    started_rx
        .recv_timeout(CHANNEL_DEADLINE)
        .expect("worker should start blocker");
    (service, blocker, release_tx)
}

/// Shuts down the service and waits for termination within the shared test
/// deadline.
fn terminate(service: &RayonExecutorService) {
    service.shutdown();
    assert!(
        service.wait_termination_timeout(CHANNEL_DEADLINE),
        "service should terminate within 10 seconds"
    );
}

#[test]
fn test_recoverable_trait_accepts_callable() {
    run_scenario("test_recoverable_trait_accepts_callable", "normal", || {
        let service = RayonExecutorService::builder()
            .num_threads(1)
            .build()
            .expect("service should build");
        let handle = attempt(&service, || Ok::<usize, io::Error>(42)).expect("shared trait should accept callable");
        terminate(&service);
        assert_eq!(handle.get().expect("callable should succeed"), 42);
    });
}

#[test]
fn test_recoverable_trait_retries_same_saturated_callable() {
    run_scenario(
        "test_recoverable_trait_retries_same_saturated_callable",
        "saturated-retry",
        || {
            let (service, blocker, release_tx) = service_with_blocker();
            let calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let task = Probe {
                payload: Box::new(42),
                calls: Arc::clone(&calls),
                drops: Arc::clone(&drops),
            };
            let payload_address = task.payload.as_ref() as *const usize;
            let rejected = match attempt(&service, task) {
                Err(rejected) => rejected,
                Ok(_) => panic!("full service unexpectedly accepted callable"),
            };
            assert_eq!(rejected.reason(), &SubmissionError::Saturated);
            let (reason, task) = rejected.into_parts();
            assert_eq!(reason, SubmissionError::Saturated);
            assert_eq!(task.payload.as_ref() as *const usize, payload_address);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(drops.load(Ordering::SeqCst), 0);

            release_tx.send(()).expect("blocker should be released");
            blocker.get().expect("blocker should complete");
            let deadline = Instant::now() + CHANNEL_DEADLINE;
            let mut task = task;
            let handle = loop {
                match attempt(&service, task) {
                    Ok(handle) => break handle,
                    Err(rejected) => {
                        let (reason, returned_task) = rejected.into_parts();
                        assert_eq!(reason, SubmissionError::Saturated);
                        assert!(Instant::now() < deadline, "capacity did not recover within 10 seconds");
                        task = returned_task;
                    }
                }
            };
            terminate(&service);
            assert_eq!(handle.get().expect("retried callable should succeed"), 42);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        },
    );
}

#[test]
fn test_recoverable_trait_shutdown_precedes_saturation() {
    run_scenario(
        "test_recoverable_trait_shutdown_precedes_saturation",
        "shutdown-precedence",
        || {
            let (service, blocker, release_tx) = service_with_blocker();
            let calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let task = Probe {
                payload: Box::new(7),
                calls: Arc::clone(&calls),
                drops: Arc::clone(&drops),
            };
            let payload_address = task.payload.as_ref() as *const usize;
            service.shutdown();
            let rejected = match attempt(&service, task) {
                Err(rejected) => rejected,
                Ok(_) => panic!("shutdown service unexpectedly accepted callable"),
            };
            assert_eq!(rejected.reason(), &SubmissionError::Shutdown);
            let (reason, task) = rejected.into_parts();
            assert_eq!(reason, SubmissionError::Shutdown);
            assert_eq!(task.payload.as_ref() as *const usize, payload_address);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            drop(task);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(drops.load(Ordering::SeqCst), 1);

            release_tx.send(()).expect("blocker should be released");
            blocker.get().expect("blocker should complete");
            assert!(
                service.wait_termination_timeout(CHANNEL_DEADLINE),
                "service should terminate within 10 seconds"
            );
        },
    );
}
