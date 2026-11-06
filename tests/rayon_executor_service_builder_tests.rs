// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Tests for [`RayonExecutorServiceBuilder`](qubit_rayon_executor::service::RayonExecutorServiceBuilder).

mod common;

use std::io;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;
use qubit_rayon_executor::RayonExecutorServiceBuildError;

#[test]
fn test_rayon_executor_service_builder_validates_configuration() {
    let zero_threads = RayonExecutorService::builder().num_threads(0).build();
    assert!(
        matches!(zero_threads, Err(RayonExecutorServiceBuildError::ZeroThreadCount),),
        "builder should reject a zero thread count",
    );

    let zero_stack = RayonExecutorService::builder().num_threads(1).stack_size(0).build();
    assert!(
        matches!(zero_stack, Err(RayonExecutorServiceBuildError::ZeroStackSize),),
        "builder should reject a zero stack size",
    );

    let zero_capacity = RayonExecutorService::builder().task_capacity(0).build();
    assert!(
        matches!(zero_capacity, Err(RayonExecutorServiceBuildError::ZeroTaskCapacity),),
        "builder should reject a zero task capacity",
    );
}

#[test]
fn test_rayon_executor_service_builder_sets_thread_options() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .thread_name_prefix("rayon-custom")
        .stack_size(2 * 1024 * 1024)
        .build()
        .expect("service should be created with custom thread options");

    let handle = service
        .submit_callable(|| {
            Ok::<_, io::Error>(
                std::thread::current()
                    .name()
                    .expect("rayon worker should be named")
                    .to_owned(),
            )
        })
        .expect("service should accept callable");

    service.shutdown();
    assert!(service.wait_termination_timeout(std::time::Duration::from_secs(10)));
    assert!(
        handle
            .get()
            .expect("callable should return worker name")
            .starts_with("rayon-custom-"),
        "worker name should use the configured prefix",
    );
}

#[test]
fn test_rayon_executor_service_uses_pool_thread_count_for_dispatch() {
    for num_threads in [1, 2] {
        let service = RayonExecutorService::builder()
            .num_threads(num_threads)
            .task_capacity(4)
            .build()
            .expect("service should be created");
        let handles = (0..4)
            .map(|value| {
                service
                    .submit_callable(move || Ok::<_, io::Error>(value))
                    .expect("service should accept work")
            })
            .collect::<Vec<_>>();

        service.shutdown();
        assert!(service.wait_termination_timeout(std::time::Duration::from_secs(10)));
        for (expected, handle) in handles.into_iter().enumerate() {
            assert_eq!(handle.get().expect("task should complete"), expected);
        }
        let stats = service.stats();
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.running, 0);
        assert_eq!(stats.cancelling, 0);
    }
}
