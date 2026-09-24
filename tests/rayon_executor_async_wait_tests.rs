// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
#![cfg(feature = "async-wait")]

use std::io;
use std::sync::mpsc;
use std::time::Duration;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;
use tokio::time::timeout;

#[tokio::test]
async fn test_rayon_await_termination_waits_for_shutdown() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let wait = service.await_termination();
    tokio::pin!(wait);
    assert!(timeout(Duration::from_millis(20), &mut wait).await.is_err());
    service.shutdown();
    timeout(Duration::from_secs(1), wait)
        .await
        .expect("empty service should terminate");
    service.await_termination().await;
}

#[tokio::test]
async fn test_rayon_await_termination_waits_for_running_task_and_multiple_waiters() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    service
        .submit(move || {
            started_tx.send(()).expect("start signal should send");
            release_rx.recv().expect("release signal should arrive");
            Ok::<(), io::Error>(())
        })
        .expect("task should be accepted");
    started_rx.recv().expect("task should start");
    service.shutdown();
    let first = service.await_termination();
    tokio::pin!(first);
    assert!(timeout(Duration::from_millis(20), &mut first).await.is_err());
    let second = service.await_termination();
    release_tx.send(()).expect("task should release");
    timeout(Duration::from_secs(1), async { tokio::join!(first, second) })
        .await
        .expect("both waiters should complete");
}

#[tokio::test]
async fn test_rayon_await_termination_survives_cancelled_waiter_and_stop() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let mut wait = Box::pin(service.await_termination());
    assert!(timeout(Duration::from_millis(20), &mut wait).await.is_err());
    drop(wait);
    service.stop();
    timeout(Duration::from_secs(1), service.await_termination())
        .await
        .expect("new waiter should complete");
}

#[tokio::test]
async fn test_rayon_await_termination_after_stop_cancels_queued_task() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    service
        .submit(move || {
            started_tx.send(()).expect("start signal should send");
            release_rx.recv().expect("release signal should arrive");
            Ok::<(), io::Error>(())
        })
        .expect("running task should be accepted");
    started_rx.recv().expect("task should start");
    service
        .submit(|| Ok::<(), io::Error>(()))
        .expect("queued task should be accepted");
    let report = service.stop();
    assert_eq!(report.cancelled, 1);
    let mut wait = Box::pin(service.await_termination());
    assert!(timeout(Duration::from_millis(20), &mut wait).await.is_err());
    release_tx.send(()).expect("task should release");
    timeout(Duration::from_secs(1), wait)
        .await
        .expect("stopped service should terminate");
}
