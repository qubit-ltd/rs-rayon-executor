/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! # Qubit Rayon Executor
//!
//! Rayon-backed CPU-bound executor service implementation.
//!
//! # Author
//!
//! Haixing Hu

mod pending_cancel;
mod rayon_executor_service;
mod rayon_executor_service_build_error;
mod rayon_executor_service_builder;
mod rayon_executor_service_state;
mod rayon_task_handle;

pub use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
    ShutdownReport,
};
pub use rayon_executor_service::RayonExecutorService;
pub use rayon_executor_service_build_error::RayonExecutorServiceBuildError;
pub use rayon_executor_service_builder::RayonExecutorServiceBuilder;
pub use rayon_task_handle::RayonTaskHandle;

/// Executor service compatibility exports for Rayon-backed users.
pub mod service {
    pub use crate::{
        ExecutorService,
        RayonExecutorService,
        RayonExecutorServiceBuildError,
        RayonExecutorServiceBuilder,
        RayonTaskHandle,
        RejectedExecution,
        ShutdownReport,
    };
}
