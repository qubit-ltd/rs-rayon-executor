// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Rayon-backed CPU-bound executor service implementation.
//!
//! The crate provides a bounded task-admission layer on top of a dedicated
//! Rayon thread pool. Use [`RayonExecutorService`] for synchronous CPU-bound
//! work, and use [`RayonTaskHandle`] when task status or cancellation is
//! required.
//!
//! # Features
//!
//! The optional `async-wait` feature is disabled by default. Enable it to use
//! the `RayonExecutorService::await_termination` and
//! `RayonExecutorService::capacity_changes` methods. Both require the
//! `async-wait` feature. The wait can be driven by any Rust future executor;
//! it does not require a Tokio runtime.
//!
//! # Examples
//!
//! ```
//! use qubit_executor::service::ExecutorService;
//! use qubit_rayon_executor::RayonExecutorService;
//!
//! let service = RayonExecutorService::builder().num_threads(1).build()?;
//! service.shutdown();
//! service.wait_termination();
//! # Ok::<(), qubit_rayon_executor::RayonExecutorServiceBuildError>(())
//! ```

#[cfg(feature = "async-wait")]
mod capacity_changes;
#[cfg(feature = "async-wait")]
mod capacity_changes_changed;
mod internal;
mod queued_job;
mod rayon_executor_service;
mod rayon_executor_service_build_error;
mod rayon_executor_service_builder;
mod rayon_executor_service_state;
mod rayon_executor_service_stats;
mod rayon_task_handle;
mod rejected_task;

#[cfg(test)]
mod tests;

#[cfg(feature = "async-wait")]
pub use capacity_changes::CapacityChanges;
#[cfg(feature = "async-wait")]
pub use capacity_changes_changed::CapacityChangesChanged;
pub use rayon_executor_service::RayonExecutorService;
pub use rayon_executor_service_build_error::RayonExecutorServiceBuildError;
pub use rayon_executor_service_builder::RayonExecutorServiceBuilder;
pub use rayon_executor_service_stats::RayonExecutorServiceStats;
pub use rayon_task_handle::RayonTaskHandle;
pub use rejected_task::RejectedTask;
