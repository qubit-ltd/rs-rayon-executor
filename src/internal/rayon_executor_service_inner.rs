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
///
/// # Invariants and ownership
///
/// The state transitions in
/// [`RayonExecutorServiceState`](crate::rayon_executor_service_state::RayonExecutorServiceState)
/// maintain these relationships while this data is locked:
///
/// - A task identifier occurs in at most one of `queue`, `running_tasks`, and
///   `cancelling`; `running` equals `running_tasks.len()`.
/// - `queue.len() + running + cancelling.len() <= task_capacity`. Capacity
///   counts accepted tasks for which the service still holds accounting
///   responsibility, regardless of whether a result handle has observed a
///   terminal result.
/// - `running <= scheduled <= num_threads`. `scheduled` counts reservations in
///   the dispatch chain, including closures that have not claimed a task and
///   reservations retained while a completed worker dispatches its successor.
///   It does not count only callables currently executing.
/// - Task identifiers increase monotonically and are never reused. When the
///   identifier space is exhausted, new admission is rejected and the lifecycle
///   moves toward shutdown; already accepted work can still finish.
/// - `admit`, `take_next`, `cancel_pending_task`, and `stop` transfer task
///   ownership between the queue and the running or cancelling sets while the
///   mutex is held. User tasks, cancellation cleanup, and asynchronous
///   notifications run after releasing the mutex. In particular, `admit` calls
///   the job's internal `accept` hook under the mutex only to mark its endpoint
///   accepted; it does not invoke the user's callable.
/// - `finish_running` and `finish_cancelling` release the service's accounting
///   responsibility. A task's terminal result may become observable before that
///   release. Termination requires admission to have stopped and the occupied
///   count to reach zero; it does not require every idle scheduled closure to
///   have exited or Rayon worker threads to have been joined.
///
/// These rules are maintained by [`RayonExecutorServiceState::admit`],
/// [`RayonExecutorServiceState::take_next`],
/// [`RayonExecutorServiceState::finish_running`],
/// [`RayonExecutorServiceState::cancel_pending_task`],
/// [`RayonExecutorServiceState::finish_cancelling`], and
/// [`RayonExecutorServiceState::stop`].
pub(crate) struct RayonExecutorServiceInner {
    /// Current lifecycle used to accept or reject new submissions.
    pub(crate) lifecycle: ExecutorServiceLifecycle,
    /// Maximum accepted tasks whose service-side execution or cancellation
    /// accounting has not yet been released.
    pub(crate) task_capacity: usize,
    /// Maximum number of Rayon worker closures scheduled at once.
    pub(crate) num_threads: usize,
    /// Next unused identifier, or `None` after the identifier space is
    /// exhausted.
    pub(crate) next_task_id: Option<u64>,
    /// Number of tasks accounted as owned by a worker, from queue removal
    /// until `finish_running` releases their service-side accounting.
    ///
    /// This interval includes a task claimed before its user code is invoked
    /// and a task whose terminal result is already observable.
    pub(crate) running: usize,
    /// Task identifiers currently accounted as owned by Rayon workers.
    pub(crate) running_tasks: BTreeSet<u64>,
    /// Number of reserved dispatch-chain closures, including closures yet to
    /// claim queued work and a worker reservation retained to dispatch a
    /// successor after completing its current task.
    pub(crate) scheduled: usize,
    /// Pending jobs indexed by their stable task identifiers.
    pub(crate) queue: BTreeMap<u64, Box<dyn QueuedJob>>,
    /// Task identifiers whose job is being cancelled outside the mutex.
    pub(crate) cancelling: BTreeSet<u64>,
}
