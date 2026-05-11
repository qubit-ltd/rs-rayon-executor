/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`RayonExecutorServiceBuilder`](qubit_rayon_executor::service::RayonExecutorServiceBuilder).

mod common;

use std::io;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::{
    RayonExecutorService,
    RayonExecutorServiceBuildError,
};

#[test]
fn test_rayon_executor_service_builder_validates_configuration() {
    let zero_threads = RayonExecutorService::builder().num_threads(0).build();
    assert!(matches!(
        zero_threads,
        Err(RayonExecutorServiceBuildError::ZeroThreadCount),
    ));

    let zero_stack = RayonExecutorService::builder()
        .num_threads(1)
        .stack_size(0)
        .build();
    assert!(matches!(
        zero_stack,
        Err(RayonExecutorServiceBuildError::ZeroStackSize),
    ));
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

    assert!(
        handle
            .get()
            .expect("callable should return worker name")
            .starts_with("rayon-custom-")
    );
    service.shutdown();
    service.wait_termination();
}
