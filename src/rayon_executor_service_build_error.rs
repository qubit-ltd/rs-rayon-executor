/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use thiserror::Error;

/// Error returned when [`crate::RayonExecutorServiceBuilder`] cannot build a service.
#[derive(Debug, Error)]
pub enum RayonExecutorServiceBuildError {
    /// The configured Rayon thread count is zero.
    #[error("rayon executor service thread count must be greater than zero")]
    ZeroThreadCount,

    /// The configured worker stack size is zero.
    #[error("rayon executor service stack size must be greater than zero")]
    ZeroStackSize,

    /// Rayon rejected the underlying thread-pool configuration.
    #[error("failed to build rayon executor service: {source}")]
    BuildFailed {
        /// Rayon build error returned by the underlying thread-pool builder.
        #[from]
        source: rayon::ThreadPoolBuildError,
    },
}
