// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use qubit_executor::service::ExecutorServiceLifecycle;

use crate::queued_job::QueuedJob;

/// Mutable data protected by the executor service-state mutex.
pub(crate) struct RayonExecutorServiceInner {
    /// Current lifecycle used to accept or reject new submissions.
    pub(crate) lifecycle: ExecutorServiceLifecycle,
    /// Maximum number of accepted tasks that have not reached a terminal state.
    pub(crate) task_capacity: usize,
    /// Maximum number of Rayon worker closures scheduled at once.
    pub(crate) num_threads: usize,
    /// Next unused identifier, or `None` after the identifier space is
    /// exhausted.
    pub(crate) next_task_id: Option<u64>,
    /// Number of jobs currently running user code.
    pub(crate) running: usize,
    /// Identifiers of jobs currently owned by Rayon workers.
    pub(crate) running_tasks: BTreeSet<u64>,
    /// Number of Rayon closures currently scheduled for queued work.
    pub(crate) scheduled: usize,
    /// Pending jobs indexed by their stable task identifiers.
    pub(crate) queue: BTreeMap<u64, Box<dyn QueuedJob>>,
    /// Task identifiers whose job is being cancelled outside the mutex.
    pub(crate) cancelling: BTreeSet<u64>,
}
