// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
#![cfg(feature = "async-wait")]

use std::future::Future;
use std::sync::Arc;
use std::task::Wake;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

struct ThreadWake(std::thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

#[test]
fn test_wait_without_tokio_runtime() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let waker = std::task::Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = Box::pin(service.await_termination());
    assert!(Future::poll(future.as_mut(), &mut cx).is_pending());

    let stopping = service.clone();
    let worker = std::thread::spawn(move || stopping.shutdown());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if Future::poll(future.as_mut(), &mut cx).is_ready() {
            break;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "termination wait exceeded deadline");
        std::thread::park_timeout(remaining);
    }
    worker.join().expect("shutdown thread should finish");
    assert!(service.is_terminated());
}
