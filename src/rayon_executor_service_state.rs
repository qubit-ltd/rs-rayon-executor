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
        Mutex, MutexGuard,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};

use qubit_atomic::AtomicCount;
use qubit_executor::service::ExecutorServiceLifecycle;
use tokio::sync::{Notify, futures::Notified};

use crate::pending_cancel::PendingCancel;

/// Shared state for [`crate::RayonExecutorService`].
pub(crate) struct RayonExecutorServiceState {
    /// Stored lifecycle state before derived termination.
    lifecycle: AtomicU8,
    /// Number of accepted tasks that have not yet completed or been cancelled.
    active_tasks: AtomicCount,
    /// Number of accepted tasks that have not started running yet.
    queued_tasks: AtomicCount,
    /// Serializes task submission and shutdown transitions.
    submission_lock: Mutex<()>,
    /// Cancellation hooks for tasks that have not started yet.
    pending_tasks: Mutex<HashMap<usize, PendingCancel>>,
    /// Monotonic identifier assigned to each accepted task.
    next_task_id: AtomicUsize,
    /// Notifies waiters once shutdown has completed and no tasks remain active.
    terminated_notify: Notify,
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
            queued_tasks: AtomicCount::new(0),
            submission_lock: Mutex::new(()),
            pending_tasks: Mutex::new(HashMap::new()),
            next_task_id: AtomicUsize::new(0),
            terminated_notify: Notify::new(),
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
        self.queued_tasks.inc();
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

    /// Marks a pending task as started if cancellation has not won first.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the task to start.
    /// * `start` - Callback that marks the task handle as started.
    ///
    /// # Returns
    ///
    /// `true` if the task was still pending and was marked as started, or `false`
    /// if it had already been cancelled or drained by shutdown.
    pub(crate) fn start_pending_task<F>(&self, task_id: usize, start: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        let mut pending_tasks = self.lock_pending_tasks();
        if !pending_tasks.contains_key(&task_id) {
            return false;
        }
        if !start() {
            return false;
        }
        pending_tasks.remove(&task_id);
        self.queued_tasks.dec();
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
    /// had already started, completed, or been drained by `stop`.
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
            self.queued_tasks.dec();
            self.active_tasks.dec() == 0
        };
        if should_notify {
            self.notify_if_terminated();
        }
        true
    }

    /// Drains pending tasks and captures a consistent shutdown snapshot.
    ///
    /// The pending-task lock serializes this snapshot with task start and manual
    /// cancellation, so `queued`, `running`, and the drained hooks describe the
    /// same observed pending state.
    ///
    /// # Returns
    ///
    /// A tuple containing the queued task count, running task count, and drained
    /// cancellation hooks.
    pub(crate) fn drain_pending_tasks_for_shutdown(&self) -> (usize, usize, Vec<PendingCancel>) {
        let mut pending_tasks = self.lock_pending_tasks();
        let queued = self.queued_tasks.get();
        let running = self.active_tasks.get().saturating_sub(queued);
        let pending = pending_tasks
            .drain()
            .map(|(_, cancel)| cancel)
            .collect::<Vec<_>>();
        debug_assert_eq!(pending.len(), queued);
        (queued, running, pending)
    }

    /// Updates counters for pending tasks drained and cancelled by shutdown.
    ///
    /// # Parameters
    ///
    /// * `pending` - Cancellation hooks drained from the pending-task map.
    ///
    /// # Returns
    ///
    /// Number of drained pending tasks whose cancellation callback succeeded.
    pub(crate) fn cancel_drained_pending_tasks(&self, pending: Vec<PendingCancel>) -> usize {
        let mut cancelled = 0usize;
        for cancel in pending {
            let was_cancelled = cancel();
            debug_assert!(
                was_cancelled,
                "drained pending rayon task should cancel before start",
            );
            if was_cancelled {
                self.queued_tasks.dec();
                self.active_tasks.dec();
                cancelled += 1;
            }
        }
        self.notify_if_terminated();
        cancelled
    }

    /// Updates counters after a started task completes.
    pub(crate) fn on_task_completed(&self) {
        if self.active_tasks.dec() == 0 {
            self.notify_if_terminated();
        }
    }

    /// Wakes termination waiters when shutdown and task completion allow it.
    pub(crate) fn notify_if_terminated(&self) {
        if self.is_not_running() && self.has_no_active_tasks() {
            self.terminated_notify.notify_waiters();
        }
    }

    /// Creates a future for the next lifecycle notification.
    ///
    /// # Returns
    ///
    /// A notification future that observes later `notify_waiters` calls even if it
    /// has not yet been polled.
    pub(crate) fn notified(&self) -> Notified<'_> {
        self.terminated_notify.notified()
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
