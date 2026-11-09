// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::io;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

struct Probe {
    payload: Box<usize>,
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl qubit_function::Callable<usize, io::Error> for Probe {
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

#[test]
fn test_saturated_rejection_returns_same_callable_and_it_can_be_retried() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocker = service
        .submit_callable(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok::<(), io::Error>(())
        })
        .expect("blocker should be accepted");
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let task = Probe {
        payload: Box::new(17),
        calls: calls.clone(),
        drops: drops.clone(),
    };
    let ptr = task.payload.as_ref() as *const usize;
    let rejected = match service.try_submit_callable(task) {
        Err(rejected) => rejected,
        Ok(_) => panic!("saturated service unexpectedly accepted the callable"),
    };
    assert_eq!(rejected.reason(), &SubmissionError::Saturated);
    assert!(!format!("{rejected:?}").contains("17"));
    let (reason, task) = rejected.into_parts();
    assert_eq!(reason, SubmissionError::Saturated);
    assert_eq!(task.payload.as_ref() as *const usize, ptr);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    release_tx.send(()).unwrap();
    blocker.get().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut task = task;
    let result = loop {
        match service.try_submit_callable(task) {
            Ok(handle) => break handle,
            Err(rejected) => {
                let (reason, returned_task) = rejected.into_parts();
                assert_eq!(reason, SubmissionError::Saturated);
                assert!(Instant::now() < deadline, "capacity did not recover within 10 seconds");
                task = returned_task;
                std::thread::yield_now();
            }
        }
    };
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(result.get().unwrap(), 17);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn test_shutdown_rejection_and_debug_do_not_require_task_debug() {
    let service = RayonExecutorService::builder().num_threads(1).build().unwrap();
    service.shutdown();
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let rejected = match service.try_submit_callable(Probe {
        payload: Box::new(29),
        calls: calls.clone(),
        drops: drops.clone(),
    }) {
        Err(rejected) => rejected,
        Ok(_) => panic!("shutdown service unexpectedly accepted the callable"),
    };
    assert_eq!(rejected.reason(), &SubmissionError::Shutdown);
    assert!(!format!("{rejected:?}").contains("29"));
    let display = format!("{rejected}");
    assert!(display.contains("shut down"));
    assert!(!display.contains("29"));
    drop(rejected);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn test_shutdown_wins_over_saturation() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = service
        .submit_callable(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok::<(), io::Error>(())
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    service.shutdown();
    let rejected = match service.try_submit_callable(|| Ok::<(), io::Error>(())) {
        Err(rejected) => rejected,
        Ok(_) => panic!("shutdown service unexpectedly accepted the callable"),
    };
    assert_eq!(rejected.reason(), &SubmissionError::Shutdown);
    drop(rejected);
    release_tx.send(()).unwrap();
    running.get().unwrap();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
}

#[test]
fn test_accepted_callable_result_error_and_panic_keep_standard_outcomes() {
    let service = RayonExecutorService::builder().num_threads(1).build().unwrap();
    let success = service.try_submit_callable(|| Ok::<usize, io::Error>(23)).unwrap();
    let failure = service
        .try_submit_callable(|| Err::<usize, _>(io::Error::other("expected failure")))
        .unwrap();
    let panic = service
        .try_submit_callable(|| -> Result<usize, io::Error> { panic!("expected panic") })
        .unwrap();
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(success.get().unwrap(), 23);
    assert!(failure.get().unwrap_err().to_string().contains("expected failure"));
    assert!(matches!(panic.get(), Err(TaskExecutionError::Panicked)));
}

struct ReentrantDrop {
    service: RayonExecutorService,
    drops: Arc<AtomicUsize>,
}

impl Drop for ReentrantDrop {
    fn drop(&mut self) {
        let _ = self.service.stats();
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn test_legacy_rejections_child() {
    if std::env::var("RXE_REJECTION_CHILD").as_deref() != Ok("1") {
        return;
    }
    legacy_rejections_scenario();
    println!("RXE_REJECTION_CHILD_OK");
}

#[test]
fn test_legacy_rejections_consume_tasks_and_drop_outside_state_lock() {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "test_legacy_rejections_child", "--nocapture"])
        .env("RXE_REJECTION_CHILD", "1");
    let result = common::subprocess::run_bounded_output(&mut command, Duration::from_secs(20))
        .expect("run legacy rejection child");
    let stdout = String::from_utf8_lossy(&result.output.stdout);
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    assert!(
        !result.timed_out && result.output.status.success(),
        "legacy rejection child: deadline=20s, timed_out={}, status={}\nstdout:\n{}\nstderr:\n{}",
        result.timed_out,
        result.output.status,
        stdout,
        stderr
    );
    assert!(
        stdout.lines().any(|line| line == "RXE_REJECTION_CHILD_OK"),
        "legacy rejection child did not complete its scenario\nstdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );
}

fn legacy_rejections_scenario() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = service
        .submit_callable(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok::<(), io::Error>(())
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));

    let probe = ReentrantDrop {
        service: service.clone(),
        drops: drops.clone(),
    };
    assert!(matches!(
        service.submit_callable(move || {
            let _keep = &probe;
            Ok::<(), io::Error>(())
        }),
        Err(SubmissionError::Saturated)
    ));
    let probe = ReentrantDrop {
        service: service.clone(),
        drops: drops.clone(),
    };
    assert!(matches!(
        service.submit_tracked_callable(move || {
            let _keep = &probe;
            Ok::<(), io::Error>(())
        }),
        Err(SubmissionError::Saturated)
    ));
    let probe = ReentrantDrop {
        service: service.clone(),
        drops: drops.clone(),
    };
    assert!(matches!(
        service.submit(move || {
            let _keep = &probe;
            Ok::<(), io::Error>(())
        }),
        Err(SubmissionError::Saturated)
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 3);

    release_tx.send(()).unwrap();
    running.get().unwrap();
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
}
