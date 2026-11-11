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
/// one consistent service-state snapshot. A snapshot is not a reservation of
/// capacity, and an observable terminal result does not mean service-side
/// accounting has been released.
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
    /// Maximum accepted tasks whose service-side execution or cancellation
    /// accounting has not yet been released. Queued, running, and cancelling
    /// tasks all consume this capacity, even if a terminal result is
    /// observable.
    pub task_capacity: usize,
    /// Accepted tasks waiting for a Rayon worker.
    pub queued: usize,
    /// Tasks claimed by Rayon workers whose running accounting has not yet
    /// been released. A task may not have invoked user code yet, or may already
    /// have published its terminal result.
    pub running: usize,
    /// Tasks removed from the queue whose cancellation is still being
    /// processed.
    pub cancelling: usize,
}
