// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::io;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_function::Callable;
use qubit_rayon_executor::RayonExecutorService;

const DEADLINE: Duration = Duration::from_secs(10);
const CHILD_MARKER: &str = "QUBIT_CALLABLE_UNWIND_CHILD";

fn service() -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .expect("service should build")
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
        .expect("isolated test child should start");
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
                let status = child.wait().expect("timed-out child should be reaped");
                panic!("isolated case {name} exceeded 10 seconds (status {status})");
            }
        }
    }
}

fn finish_with_successor(service: &RayonExecutorService, runs: Arc<AtomicUsize>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let observed = Arc::clone(&runs);
        match service.submit_callable(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok::<(), io::Error>(())
        }) {
            Ok(handle) => {
                service.shutdown();
                assert!(service.wait_termination_timeout(DEADLINE));
                handle.get().expect("successor should finish");
                assert_eq!(runs.load(Ordering::SeqCst), 1);
                let stats = service.stats();
                assert_eq!((stats.queued, stats.running, stats.cancelling), (0, 0, 0));
                return;
            }
            Err(SubmissionError::Saturated) if Instant::now() < deadline => std::thread::yield_now(),
            Err(error) => panic!("successor submission failed: {error}"),
        }
    }
}

struct DropProbe {
    drops: Arc<AtomicUsize>,
    panic_on_drop: bool,
}

impl Callable<usize, io::Error> for DropProbe {
    fn call(&mut self) -> Result<usize, io::Error> {
        Ok(7)
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        assert!(!self.panic_on_drop, "intentional callable destructor panic");
    }
}

#[test]
fn test_ordinary_callable_panic_runs_hook_once_and_preserves_progress() {
    run_isolated(
        "test_ordinary_callable_panic_runs_hook_once_and_preserves_progress",
        ordinary_panic_case,
    );
}

fn ordinary_panic_case() {
    let hook_count = Arc::new(AtomicUsize::new(0));
    let hook_counter = Arc::clone(&hook_count);
    std::panic::set_hook(Box::new(move |_| {
        hook_counter.fetch_add(1, Ordering::SeqCst);
    }));
    let service = service();
    let handle = service
        .submit_callable(|| -> Result<(), io::Error> { panic!("ordinary callable panic") })
        .expect("panic task should be accepted");
    assert!(matches!(handle.get(), Err(TaskExecutionError::Panicked)));
    let observed_hooks = hook_count.load(Ordering::SeqCst);
    if observed_hooks != 1 {
        eprintln!("ordinary callable panic invoked panic hook {observed_hooks} times");
    }
    assert_eq!(observed_hooks, 1);
    finish_with_successor(&service, Arc::new(AtomicUsize::new(0)));
}

#[test]
fn test_callable_drop_panic_result_handle_classification_and_progress() {
    run_isolated(
        "test_callable_drop_panic_result_handle_classification_and_progress",
        result_handle_drop_panic_case,
    );
}

fn result_handle_drop_panic_case() {
    let service = service();
    let drops = Arc::new(AtomicUsize::new(0));
    let result = service
        .submit_callable(DropProbe {
            drops: Arc::clone(&drops),
            panic_on_drop: true,
        })
        .expect("callable should be accepted")
        .get();
    assert!(
        matches!(result, Err(TaskExecutionError::Panicked)),
        "actual result: {result:?}"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    finish_with_successor(&service, Arc::new(AtomicUsize::new(0)));
}

#[test]
fn test_callable_drop_panic_tracked_handle_classification_and_progress() {
    run_isolated(
        "test_callable_drop_panic_tracked_handle_classification_and_progress",
        tracked_handle_drop_panic_case,
    );
}

fn tracked_handle_drop_panic_case() {
    let service = service();
    let drops = Arc::new(AtomicUsize::new(0));
    let result = service
        .submit_tracked_callable(DropProbe {
            drops: Arc::clone(&drops),
            panic_on_drop: true,
        })
        .expect("callable should be accepted")
        .get();
    assert!(
        matches!(result, Err(TaskExecutionError::Panicked)),
        "actual result: {result:?}"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    finish_with_successor(&service, Arc::new(AtomicUsize::new(0)));
}

#[test]
fn test_callable_normal_drop_returns_value_and_preserves_progress() {
    run_isolated(
        "test_callable_normal_drop_returns_value_and_preserves_progress",
        normal_drop_case,
    );
}

fn normal_drop_case() {
    let service = service();
    let drops = Arc::new(AtomicUsize::new(0));
    let value = service
        .submit_callable(DropProbe {
            drops: Arc::clone(&drops),
            panic_on_drop: false,
        })
        .expect("callable should be accepted")
        .get()
        .expect("normal callable should succeed");
    assert_eq!(value, 7);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    finish_with_successor(&service, Arc::new(AtomicUsize::new(0)));
}
