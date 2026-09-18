// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use thiserror::Error;

/// Error returned when [`crate::RayonExecutorServiceBuilder`] cannot build a
/// service.
///
/// # Examples
///
/// ```
/// use qubit_rayon_executor::RayonExecutorService;
/// use qubit_rayon_executor::RayonExecutorServiceBuildError;
///
/// let error = RayonExecutorService::builder().num_threads(0).build();
/// assert!(matches!(error, Err(RayonExecutorServiceBuildError::ZeroThreadCount)));
/// ```
#[derive(Debug, Error)]
pub enum RayonExecutorServiceBuildError {
    /// Indicates that the configured Rayon thread count is zero.
    #[error("rayon executor service thread count must be greater than zero")]
    ZeroThreadCount,

    /// Indicates that the configured worker stack size is zero.
    #[error("rayon executor service stack size must be greater than zero")]
    ZeroStackSize,

    /// Indicates that the configured accepted-task capacity is zero.
    #[error("rayon executor service task capacity must be greater than zero")]
    ZeroTaskCapacity,

    /// Wraps Rayon rejecting the underlying thread-pool configuration.
    #[error("failed to build rayon executor service: {source}")]
    BuildFailed {
        /// Rayon build error returned by the underlying thread-pool builder.
        #[from]
        source: rayon::ThreadPoolBuildError,
    },
}
