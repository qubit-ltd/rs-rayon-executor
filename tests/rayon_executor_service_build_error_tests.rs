/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`RayonExecutorServiceBuildError`](qubit_rayon_executor::service::RayonExecutorServiceBuildError).

use std::error::Error;

use qubit_rayon_executor::{
    RayonExecutorService,
    RayonExecutorServiceBuildError,
};

/// Extracts the build error from a builder result.
fn expect_build_error(
    result: Result<RayonExecutorService, RayonExecutorServiceBuildError>,
    message: &str,
) -> RayonExecutorServiceBuildError {
    match result {
        Ok(_) => panic!("{message}"),
        Err(err) => err,
    }
}

#[test]
fn test_rayon_executor_service_build_error_zero_thread_count_display() {
    let err = expect_build_error(
        RayonExecutorService::builder().num_threads(0).build(),
        "zero thread count should be rejected",
    );

    assert!(matches!(
        err,
        RayonExecutorServiceBuildError::ZeroThreadCount,
    ));
    assert_eq!(
        err.to_string(),
        "rayon executor service thread count must be greater than zero",
    );
    assert!(
        err.source().is_none(),
        "validation error should not wrap a source error",
    );
}

#[test]
fn test_rayon_executor_service_build_error_zero_stack_size_display() {
    let err = expect_build_error(
        RayonExecutorService::builder()
            .num_threads(1)
            .stack_size(0)
            .build(),
        "zero stack size should be rejected",
    );

    assert!(matches!(err, RayonExecutorServiceBuildError::ZeroStackSize));
    assert_eq!(
        err.to_string(),
        "rayon executor service stack size must be greater than zero",
    );
    assert!(
        err.source().is_none(),
        "validation error should not wrap a source error",
    );
}
