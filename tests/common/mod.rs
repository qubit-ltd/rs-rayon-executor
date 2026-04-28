/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
#![allow(dead_code)]

use std::{
    io,
    sync::mpsc,
    time::Duration,
};

use qubit_rayon_executor::RayonExecutorService;

pub(crate) fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

pub(crate) fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

pub(crate) fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime for rayon executor service tests")
}

pub(crate) fn create_single_worker_service() -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("rayon executor service should be created")
}

pub(crate) fn wait_started(receiver: mpsc::Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should start within timeout");
}

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
