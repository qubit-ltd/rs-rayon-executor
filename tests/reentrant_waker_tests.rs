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
use std::pin::Pin;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Wake;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

struct ReentrantWake {
    service: RayonExecutorService,
    fired: AtomicBool,
    action: &'static str,
    fired_tx: std::sync::mpsc::Sender<()>,
}

struct CountWake(AtomicUsize);

impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Wake for ReentrantWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.fired.swap(true, Ordering::SeqCst) {
            match self.action {
                "stats" => {
                    let _ = self.service.stats();
                }
                "submit" => {
                    assert!(matches!(
                        self.service.submit(|| Ok::<(), io::Error>(())),
                        Err(SubmissionError::Shutdown)
                    ));
                }
                "shutdown" => self.service.shutdown(),
                "stop" => {
                    let _ = self.service.stop();
                }
                _ => panic!("unknown reentrant action: {}", self.action),
            }
            let _ = self.fired_tx.send(());
        }
    }
}

#[test]
fn test_reentrant_waker_reentry_completes() {
    if let Some(case) = std::env::var_os("RXE_REENTRANT_CASE") {
        run_case(case.to_string_lossy().as_ref());
        return;
    }
    let cases = ["capacity", "termination"]
        .into_iter()
        .flat_map(|signal| {
            ["shutdown", "stop"].into_iter().flat_map(move |trigger| {
                ["stats", "submit", "shutdown", "stop"]
                    .into_iter()
                    .map(move |action| format!("{signal}-{trigger}-{action}"))
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .chain(["completion-control-stats".to_owned(), "queued-control-stats".to_owned()])
        .collect::<Vec<_>>();
    for case in cases {
        let mut child = Command::new(std::env::current_exe().expect("test executable path"))
            .args(["--exact", "test_reentrant_waker_reentry_completes", "--nocapture"])
            .env("RXE_REENTRANT_CASE", &case)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("child test should start");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().expect("child status should be readable") {
                let output = child.wait_with_output().expect("child output should be readable");
                assert!(
                    status.success(),
                    "child case {case} failed: {status}\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().expect("timed out child should be killed");
                let status = child.wait().expect("killed child should be reaped");
                let output = child
                    .wait_with_output()
                    .expect("killed child output should be readable");
                panic!(
                    "child case {case} timed out and was killed ({status})\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn test_late_termination_subscriber_does_not_rewake_existing_waiter() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let count = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker = std::task::Waker::from(Arc::clone(&count));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut first = Box::pin(service.await_termination());
    assert!(Future::poll(first.as_mut(), &mut cx).is_pending());

    service.shutdown();
    let wakes_after_termination = count.0.load(Ordering::SeqCst);
    assert!(wakes_after_termination > 0, "termination should wake the first waiter");

    let mut late = Box::pin(service.await_termination());
    assert!(Future::poll(late.as_mut(), &mut cx).is_ready());
    assert_eq!(
        count.0.load(Ordering::SeqCst),
        wakes_after_termination,
        "late subscription must not republish an already-published terminal value"
    );
}

fn run_case(case: &str) {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let mut parts = case.split('-');
    let signal = parts.next().expect("signal is present");
    let trigger = parts.next().expect("trigger is present");
    let action = match parts.next().expect("action is present") {
        "stats" => "stats",
        "submit" => "submit",
        "shutdown" => "shutdown",
        "stop" => "stop",
        _ => panic!("unknown reentrant action in child case: {case}"),
    };
    assert!(parts.next().is_none(), "case should have three fields");
    let (fired_tx, fired_rx) = std::sync::mpsc::channel();
    let waker = std::task::Waker::from(Arc::new(ReentrantWake {
        service: service.clone(),
        fired: AtomicBool::new(false),
        action,
        fired_tx,
    }));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut waiter: Option<Pin<Box<dyn Future<Output = ()> + '_>>>;
    match signal {
        "capacity" => {
            let mut receiver = service.capacity_changes();
            waiter = Some(Box::pin(async move {
                receiver.changed().await.expect("capacity watch should remain open");
            }));
            assert!(Future::poll(waiter.as_mut().unwrap().as_mut(), &mut cx).is_pending());
        }
        "termination" => {
            waiter = Some(Box::pin(service.await_termination()));
            assert!(Future::poll(waiter.as_mut().unwrap().as_mut(), &mut cx).is_pending());
        }
        "completion" | "queued" => {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            service
                .submit(move || {
                    started_tx.send(()).expect("blocker should signal start");
                    release_rx
                        .recv_timeout(Duration::from_secs(4))
                        .expect("blocker should be released");
                    Ok::<(), io::Error>(())
                })
                .expect("blocker should be accepted");
            started_rx
                .recv_timeout(Duration::from_secs(4))
                .expect("blocker should start");
            let mut receiver = service.capacity_changes();
            waiter = Some(Box::pin(async move {
                receiver.changed().await.expect("capacity watch should remain open");
            }));
            assert!(Future::poll(waiter.as_mut().unwrap().as_mut(), &mut cx).is_pending());
            if signal == "completion" {
                release_tx.send(()).expect("blocker should be released");
            } else {
                let queued = service
                    .submit_tracked_callable(|| Ok::<(), io::Error>(()))
                    .expect("queued task should be accepted");
                let _ = queued.cancel();
                release_tx.send(()).expect("blocker should be released");
            }
            fired_rx
                .recv_timeout(Duration::from_secs(4))
                .expect("capacity wake should reenter and finish");
            assert!(Future::poll(waiter.as_mut().unwrap().as_mut(), &mut cx).is_ready());
            service.shutdown();
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            while service.stats().running != 0 {
                assert!(std::time::Instant::now() < deadline, "blocker should complete");
                std::thread::yield_now();
            }
            assert!(service.is_terminated(), "controlled work should be complete");
            return;
        }
        _ => panic!("unknown signal in child case: {case}"),
    }
    match trigger {
        "shutdown" => service.shutdown(),
        "stop" => {
            let _ = service.stop();
        }
        _ => panic!("unknown trigger in child case: {case}"),
    }
    fired_rx
        .recv_timeout(Duration::from_secs(4))
        .expect("watch notification should invoke the reentrant callback");
    assert!(Future::poll(waiter.as_mut().unwrap().as_mut(), &mut cx).is_ready());
}
