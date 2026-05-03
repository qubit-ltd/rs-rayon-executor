/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
#![allow(dead_code)]

use std::{
    io,
    sync::mpsc,
    time::Duration,
};

use qubit_rayon_executor::RayonExecutorService;

/// Returns a successful unit task result for executor submission tests.
pub(crate) fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

/// Returns a successful integer task result for callable executor tests.
pub(crate) fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

/// Creates a current-thread Tokio runtime for awaiting executor shutdown.
pub(crate) fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime for rayon executor service tests")
}

/// Creates a Rayon executor service with a single worker thread.
pub(crate) fn create_single_worker_service() -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("rayon executor service should be created")
}

/// Waits until a task signals that it has started.
pub(crate) fn wait_started(receiver: mpsc::Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should start within timeout");
}

/// Polls a condition until it becomes true or the timeout expires.
pub(crate) fn wait_until<F>(mut condition: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "condition should become true within timeout");
}
