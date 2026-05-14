/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        MutexGuard,
        atomic::{
            AtomicU8,
            AtomicUsize,
            Ordering,
        },
    },
};

use qubit_atomic::AtomicCount;
use qubit_executor::service::{
    ExecutorServiceLifecycle,
    StopReport,
};
use qubit_lock::Monitor;

use crate::pending_cancel::PendingCancel;

/// Shared state for [`crate::RayonExecutorService`].
pub(crate) struct RayonExecutorServiceState {
    /// Stored lifecycle state before derived termination.
    lifecycle: AtomicU8,
    /// Number of accepted tasks that have not yet completed or been cancelled.
    active_tasks: AtomicCount,
    /// Serializes task submission and shutdown transitions.
    submission_lock: Mutex<()>,
    /// Cancellation hooks for tasks that have not started yet.
    pending_tasks: Mutex<HashMap<usize, PendingCancel>>,
    /// Monotonic identifier assigned to each accepted task.
    next_task_id: AtomicUsize,
    /// Published termination condition for blocking waiters.
    terminated: Monitor<bool>,
}

impl RayonExecutorServiceState {
    /// Creates fresh shared state for a Rayon-backed executor service.
    ///
    /// # Returns
    ///
    /// Shared service state with zero accepted tasks and running lifecycle.
    pub(crate) fn new() -> Self {
        Self {
            lifecycle: AtomicU8::new(ExecutorServiceLifecycle::Running as u8),
            active_tasks: AtomicCount::new(0),
            submission_lock: Mutex::new(()),
            pending_tasks: Mutex::new(HashMap::new()),
            next_task_id: AtomicUsize::new(0),
            terminated: Monitor::new(false),
        }
    }

    /// Acquires the submission lock while tolerating poisoned locks.
    ///
    /// # Returns
    ///
    /// A guard for the submission lock.
    pub(crate) fn lock_submission(&self) -> MutexGuard<'_, ()> {
        self.submission_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquires the pending-task map while tolerating poisoned locks.
    ///
    /// # Returns
    ///
    /// A guard for the pending-task cancellation map.
    fn lock_pending_tasks(&self) -> MutexGuard<'_, HashMap<usize, PendingCancel>> {
        self.pending_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Returns the stored lifecycle state.
    fn stored_lifecycle(&self) -> ExecutorServiceLifecycle {
        lifecycle_from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    /// Returns the observed lifecycle state.
    pub(crate) fn lifecycle(&self) -> ExecutorServiceLifecycle {
        let lifecycle = self.stored_lifecycle();
        if lifecycle != ExecutorServiceLifecycle::Running && self.has_no_active_tasks() {
            ExecutorServiceLifecycle::Terminated
        } else {
            lifecycle
        }
    }

    /// Returns whether shutdown or stop has been requested.
    pub(crate) fn is_not_running(&self) -> bool {
        self.stored_lifecycle() != ExecutorServiceLifecycle::Running
    }

    /// Marks the service as shutting down.
    pub(crate) fn shutdown(&self) {
        let _ = self
            .lifecycle
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (lifecycle_from_u8(current) == ExecutorServiceLifecycle::Running)
                    .then_some(ExecutorServiceLifecycle::ShuttingDown as u8)
            });
    }

    /// Marks the service as stopping.
    pub(crate) fn stop(&self) {
        self.lifecycle
            .store(ExecutorServiceLifecycle::Stopping as u8, Ordering::Release);
    }

    /// Returns whether no accepted task remains active.
    pub(crate) fn has_no_active_tasks(&self) -> bool {
        self.active_tasks.is_zero()
    }

    /// Allocates the next stable task identifier.
    ///
    /// # Returns
    ///
    /// A unique task identifier within this service instance.
    pub(crate) fn next_task_id(&self) -> usize {
        self.next_task_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Records one accepted task.
    pub(crate) fn on_task_accepted(&self) {
        self.active_tasks.inc();
    }

    /// Registers a pending task cancellation hook.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the accepted task.
    /// * `cancel` - Callback used to cancel the task before it starts.
    pub(crate) fn register_pending_task(&self, task_id: usize, cancel: PendingCancel) {
        self.lock_pending_tasks().insert(task_id, cancel);
    }

    /// Claims a pending task for execution if cancellation has not won first.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the task to claim.
    /// * `claim` - Callback that decides whether the pending task can be
    ///   claimed before it is removed from the pending-task map.
    ///
    /// # Returns
    ///
    /// `true` if the task was still pending and was claimed for execution, or
    /// `false` if it had already been cancelled or stopped.
    pub(crate) fn start_pending_task<F>(&self, task_id: usize, claim: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        let mut pending_tasks = self.lock_pending_tasks();
        if !pending_tasks.contains_key(&task_id) {
            return false;
        }
        if !claim() {
            return false;
        }
        pending_tasks.remove(&task_id);
        true
    }

    /// Cancels a pending task if no Rayon worker has started it yet.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the task to cancel.
    /// * `cancel` - Callback that publishes cancellation to the task handle.
    ///
    /// # Returns
    ///
    /// `true` if the pending task was cancelled by this call, or `false` if it
    /// had already started, completed, or been cancelled by `stop`.
    pub(crate) fn cancel_pending_task(&self, task_id: usize, cancel: &PendingCancel) -> bool {
        let should_notify = {
            let mut pending_tasks = self.lock_pending_tasks();
            if !pending_tasks.contains_key(&task_id) {
                return false;
            }
            if !cancel() {
                return false;
            }
            pending_tasks.remove(&task_id);
            self.active_tasks.dec() == 0
        };
        if should_notify {
            self.notify_if_terminated();
        }
        true
    }

    /// Cancels pending tasks and captures a consistent stop report.
    ///
    /// The pending-task lock serializes cancellation with task start and manual
    /// cancellation. This keeps the pending-task map and active counter in sync
    /// for concurrent `stop` calls.
    ///
    /// # Returns
    ///
    /// A report containing the queued and running task counts observed before
    /// cancellation, plus the number of pending tasks cancelled by this call.
    pub(crate) fn cancel_pending_tasks_for_stop(&self) -> StopReport {
        let (report, should_notify) = {
            let mut pending_tasks = self.lock_pending_tasks();
            let queued = pending_tasks.len();
            let running = self.active_tasks.get().saturating_sub(queued);

            let mut cancelled = 0usize;
            for (_, cancel) in pending_tasks.drain() {
                let was_cancelled = cancel();
                debug_assert!(
                    was_cancelled,
                    "drained pending rayon task should cancel before start",
                );
                if was_cancelled {
                    self.active_tasks.dec();
                    cancelled += 1;
                }
            }

            (
                StopReport::new(queued, running, cancelled),
                self.has_no_active_tasks(),
            )
        };

        if should_notify {
            self.notify_if_terminated();
        }
        report
    }

    /// Updates counters after a started task completes.
    pub(crate) fn on_task_completed(&self) {
        if self.active_tasks.dec() == 0 {
            self.notify_if_terminated();
        }
    }

    /// Blocks until shutdown or stop has completed and no tasks remain active.
    pub(crate) fn wait_for_termination(&self) {
        self.terminated.wait_until(|terminated| *terminated, |_| ());
    }

    /// Publishes termination and wakes waiters when no task remains active.
    pub(crate) fn notify_if_terminated(&self) {
        if self.is_not_running() && self.has_no_active_tasks() {
            self.terminated.write(|terminated| *terminated = true);
            self.terminated.notify_all();
        }
    }
}

fn lifecycle_from_u8(value: u8) -> ExecutorServiceLifecycle {
    match value {
        0 => ExecutorServiceLifecycle::Running,
        1 => ExecutorServiceLifecycle::ShuttingDown,
        2 => ExecutorServiceLifecycle::Stopping,
        _ => ExecutorServiceLifecycle::Terminated,
    }
}
