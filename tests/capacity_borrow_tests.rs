// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Tests for copied capacity generations and watch borrow lock boundaries.
#![cfg(feature = "async-wait")]

use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

#[test]
fn test_copied_generation_methods_preserve_watch_seen_semantics() {
    let service = RayonExecutorService::new().expect("service should be created");
    let mut changes = service.capacity_changes();

    assert_eq!(changes.generation(), 0);
    assert!(!changes.has_changed().expect("receiver should be open"));

    service.shutdown();
    assert!(changes.has_changed().expect("receiver should be open"));
    let generation = changes.generation();
    assert!(generation > 0);
    assert!(changes.has_changed().expect("generation must remain unseen"));

    assert_eq!(changes.generation_and_update(), generation);
    assert!(!changes.has_changed().expect("generation should now be seen"));
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
}

#[test]
fn test_copied_generation_does_not_hold_shutdown_notification_lock() {
    let service = RayonExecutorService::new().expect("service should be created");
    let changes = service.capacity_changes();

    let _generation = changes.generation();
    service.shutdown();

    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
}

#[test]
fn test_borrowed_generation_blocks_shutdown_notification_in_isolated_process() {
    const CHILD_ENV: &str = "RX001_CAPACITY_BORROW_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let service = RayonExecutorService::new().expect("service should be created");
        let changes = service.capacity_changes();
        let guard = changes.borrow();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let shutdown_service = service.clone();
        let shutdown_thread = thread::spawn(move || {
            started_tx.send(()).expect("parent test should be listening");
            shutdown_service.shutdown();
            finished_tx.send(()).expect("parent test should be listening");
        });

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("shutdown thread should start");
        let lifecycle_deadline = Instant::now() + Duration::from_secs(2);
        while !service.is_terminated() {
            assert!(
                Instant::now() < lifecycle_deadline,
                "shutdown should transition the service before notification publication"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(200)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!changes.has_changed().expect("receiver should remain open"));

        drop(guard);
        finished_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("shutdown should finish after the borrow guard is released");
        assert!(changes.has_changed().expect("receiver should remain open"));
        shutdown_thread.join().expect("shutdown thread should join");
        assert!(service.wait_termination_timeout(Duration::from_secs(10)));
        return;
    }

    let executable = std::env::current_exe().expect("test executable path should be available");
    let mut child = Command::new(executable)
        .arg("--exact")
        .arg("test_borrowed_generation_blocks_shutdown_notification_in_isolated_process")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .spawn()
        .expect("child test process should start");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            assert!(status.success(), "child test failed with {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("timed out child should be killed");
            let _ = child.wait();
            panic!("child test exceeded its 10 second deadline");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
