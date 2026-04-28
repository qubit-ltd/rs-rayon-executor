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

mod rayon_executor_service;

pub use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
    ShutdownReport,
};
pub use rayon_executor_service::{
    RayonExecutorService,
    RayonExecutorServiceBuildError,
    RayonExecutorServiceBuilder,
    RayonTaskHandle,
};

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
