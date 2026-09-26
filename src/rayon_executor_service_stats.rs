// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Best-effort snapshot of a Rayon executor service.

use qubit_executor::service::ExecutorServiceLifecycle;

/// Current queue and worker counts for a Rayon executor service.
///
/// The fields are sampled while holding the service state mutex and describe
/// one consistent service-state snapshot.
///
/// # Examples
///
/// ```
/// use qubit_rayon_executor::RayonExecutorService;
/// use qubit_executor::service::ExecutorService;
///
/// let service = RayonExecutorService::new().expect("service should build");
/// let stats = service.stats();
/// assert!(stats.task_capacity > 0);
/// service.shutdown();
/// service.wait_termination();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RayonExecutorServiceStats {
    /// Observed service lifecycle.
    pub lifecycle: ExecutorServiceLifecycle,
    /// Maximum number of accepted tasks that have not reached a terminal state.
    pub task_capacity: usize,
    /// Accepted tasks waiting for a Rayon worker.
    pub queued: usize,
    /// Tasks currently running on Rayon workers.
    pub running: usize,
    /// Tasks removed from the queue whose cancellation is still being
    /// processed.
    pub cancelling: usize,
}
