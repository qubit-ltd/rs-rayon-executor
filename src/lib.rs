/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! # Qubit Rayon Executor
//!
//! Rayon-backed CPU-bound executor service implementation.
//!

mod pending_cancel;
mod rayon_executor_service;
mod rayon_executor_service_build_error;
mod rayon_executor_service_builder;
mod rayon_executor_service_state;
mod rayon_task_handle;

pub use qubit_executor::service::{
    ExecutorService,
    ExecutorServiceLifecycle,
    StopReport,
    SubmissionError,
};
pub use qubit_executor::task::spi::{
    TaskResultHandle,
    TrackedTaskHandle,
};
pub use qubit_executor::{
    CancelResult,
    TaskHandle,
    TaskResult,
    TaskStatus,
    TryGet,
};
pub use rayon_executor_service::RayonExecutorService;
pub use rayon_executor_service_build_error::RayonExecutorServiceBuildError;
pub use rayon_executor_service_builder::RayonExecutorServiceBuilder;
pub use rayon_task_handle::RayonTaskHandle;

/// Executor service compatibility exports for Rayon-backed users.
pub mod service {
    pub use crate::{
        CancelResult,
        ExecutorService,
        ExecutorServiceLifecycle,
        RayonExecutorService,
        RayonExecutorServiceBuildError,
        RayonExecutorServiceBuilder,
        RayonTaskHandle,
        StopReport,
        SubmissionError,
        TaskHandle,
        TaskResult,
        TaskResultHandle,
        TaskStatus,
        TrackedTaskHandle,
        TryGet,
    };
}
