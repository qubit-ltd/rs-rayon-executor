// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use parking_lot::Condvar;
use parking_lot::Mutex;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
#[cfg(feature = "async-wait")]
use tokio::sync::watch;

use crate::internal::Admission;
use crate::internal::CancelDisposition;
use crate::internal::RayonExecutorServiceInner;
use crate::queued_job::QueuedJob;

/// Collection of queued jobs that must be cancelled after releasing the lock.
type OwnedQueuedJobs = Vec<(usize, Box<dyn QueuedJob>)>;

/// Shared state for the Rayon executor service.
pub(crate) struct RayonExecutorServiceState {
    /// Mutex protecting lifecycle counters and queued jobs.
    inner: Mutex<RayonExecutorServiceInner>,
    /// Condition variable notified when accepted work reaches termination.
    terminated: Condvar,
    /// Monotonic notification for asynchronous termination waiters.
    #[cfg(feature = "async-wait")]
    termination_tx: watch::Sender<bool>,
}

impl RayonExecutorServiceState {
    /// Creates shared state for a bounded Rayon executor.
    ///
    /// # Parameters
    ///
    /// * `task_capacity` - Maximum accepted tasks that have not terminated.
    /// * `num_threads` - Maximum worker closures scheduled concurrently.
    ///
    /// # Returns
    ///
    /// Reference-counted state initialized in the running lifecycle.
    pub(crate) fn new(task_capacity: usize, num_threads: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RayonExecutorServiceInner {
                lifecycle: ExecutorServiceLifecycle::Running,
                task_capacity,
                num_threads,
                next_task_id: 0,
                running: 0,
                scheduled: 0,
                queue: BTreeMap::new(),
                cancelling: BTreeSet::new(),
            }),
            terminated: Condvar::new(),
            #[cfg(feature = "async-wait")]
            termination_tx: watch::channel(false).0,
        })
    }

    /// Subscribes to termination under the same lock as state transitions.
    #[cfg(feature = "async-wait")]
    pub(crate) fn subscribe_termination(&self) -> watch::Receiver<bool> {
        let inner = self.inner.lock();
        if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(&inner) == 0 {
            self.termination_tx.send_replace(true);
        }
        self.termination_tx.subscribe()
    }

    /// Accepts a queued job if lifecycle and capacity permit it.
    ///
    /// The job is marked accepted while the state mutex is held, preventing a
    /// worker from observing an unaccepted result endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] when not running and
    /// [`SubmissionError::Saturated`] when the accepted-task limit is reached.
    pub(crate) fn admit(&self, job: Box<dyn QueuedJob>) -> Result<Admission, SubmissionError> {
        let mut inner = self.inner.lock();
        if inner.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(SubmissionError::Shutdown);
        }
        if occupied(&inner) >= inner.task_capacity {
            return Err(SubmissionError::Saturated);
        }
        let task_id = inner.next_task_id;
        inner.next_task_id = inner.next_task_id.checked_add(1).expect("task id exhausted");
        job.accept();
        inner.queue.insert(task_id, job);
        let dispatch_count = inner.queue.len().min(inner.num_threads.saturating_sub(inner.scheduled));
        inner.scheduled += dispatch_count;
        debug_assert!(inner.scheduled <= inner.num_threads);
        Ok(Admission {
            task_id,
            dispatch_count,
        })
    }

    /// Takes one queued job for a scheduled Rayon closure.
    ///
    /// # Returns
    ///
    /// The oldest queued job, or `None` when a previously scheduled closure
    /// finds no work and releases its scheduling reservation.
    pub(crate) fn take_next(&self) -> Option<Box<dyn QueuedJob>> {
        let mut inner = self.inner.lock();
        let Some((_, job)) = inner.queue.pop_first() else {
            inner.scheduled = inner.scheduled.saturating_sub(1);
            self.notify_if_terminated_locked(&inner);
            return None;
        };
        inner.running += 1;
        Some(job)
    }

    /// Finishes one running task and returns whether a successor closure is
    /// needed.
    ///
    /// # Returns
    ///
    /// `true` when queued work needs the same worker reservation to run next.
    pub(crate) fn finish_running(&self) -> bool {
        let mut inner = self.inner.lock();
        inner.running = inner.running.saturating_sub(1);
        let dispatch = !inner.queue.is_empty();
        if !dispatch {
            inner.scheduled = inner.scheduled.saturating_sub(1);
        }
        self.notify_if_terminated_locked(&inner);
        dispatch
    }

    /// Claims a pending task for cancellation.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the task whose handle requested
    ///   cancellation.
    ///
    /// # Returns
    ///
    /// The ownership disposition that tells the caller whether it must cancel
    /// a queued job after releasing the mutex.
    pub(crate) fn cancel_pending_task(&self, task_id: usize) -> CancelDisposition {
        let mut inner = self.inner.lock();
        if let Some(job) = inner.queue.remove(&task_id) {
            inner.cancelling.insert(task_id);
            return CancelDisposition::Owned(job);
        }
        if inner.cancelling.contains(&task_id) {
            CancelDisposition::InProgress
        } else {
            CancelDisposition::Absent
        }
    }

    /// Completes an out-of-lock cancellation operation.
    ///
    /// This removes the cancellation reservation and wakes any termination
    /// waiter that can now observe all accepted tasks in terminal states.
    pub(crate) fn finish_cancelling(&self, task_id: usize) {
        let mut inner = self.inner.lock();
        inner.cancelling.remove(&task_id);
        self.notify_if_terminated_locked(&inner);
        self.terminated.notify_all();
    }

    /// Changes the lifecycle to graceful shutdown.
    ///
    /// Existing work remains runnable; repeated calls preserve the existing
    /// stopping lifecycle rather than changing it back.
    pub(crate) fn shutdown(&self) {
        let mut inner = self.inner.lock();
        if inner.lifecycle == ExecutorServiceLifecycle::Running {
            inner.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        self.notify_if_terminated_locked(&inner);
    }

    /// Stops admission and takes all still queued jobs for cancellation.
    ///
    /// # Returns
    ///
    /// A stop report and jobs that the caller must cancel outside this mutex to
    /// avoid executing user cleanup while holding service state.
    pub(crate) fn stop(&self) -> (StopReport, OwnedQueuedJobs) {
        let mut inner = self.inner.lock();
        inner.lifecycle = ExecutorServiceLifecycle::Stopping;
        let queued = inner.queue.len();
        let running = inner.running;
        let jobs = std::mem::take(&mut inner.queue);
        let mut owned = Vec::with_capacity(jobs.len());
        for (task_id, job) in jobs {
            inner.cancelling.insert(task_id);
            owned.push((task_id, job));
        }
        self.notify_if_terminated_locked(&inner);
        (StopReport::new(queued, running, owned.len()), owned)
    }

    /// Returns the current lifecycle, deriving termination from task counters.
    ///
    /// # Returns
    ///
    /// `Terminated` after shutdown or stop when no accepted task remains;
    /// otherwise the current nonterminal lifecycle.
    pub(crate) fn lifecycle(&self) -> ExecutorServiceLifecycle {
        let inner = self.inner.lock();
        if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(&inner) == 0 {
            ExecutorServiceLifecycle::Terminated
        } else {
            inner.lifecycle
        }
    }

    /// Returns whether new submissions are rejected.
    ///
    /// # Returns
    ///
    /// `true` once graceful shutdown or stop has begun.
    pub(crate) fn is_not_running(&self) -> bool {
        self.inner.lock().lifecycle != ExecutorServiceLifecycle::Running
    }

    /// Waits until lifecycle termination.
    ///
    /// This blocks until shutdown or stop was requested and every accepted task
    /// has completed or been cancelled.
    pub(crate) fn wait_for_termination(&self) {
        let mut guard = self.inner.lock();
        while guard.lifecycle == ExecutorServiceLifecycle::Running || occupied(&guard) != 0 {
            self.terminated.wait(&mut guard);
        }
    }

    /// Waits up to the supplied duration for lifecycle termination.
    ///
    /// # Parameters
    ///
    /// * `timeout` - Maximum time to block while observing state changes.
    ///
    /// # Returns
    ///
    /// `true` after termination is observed; `false` when the timeout expires.
    pub(crate) fn wait_for_termination_timeout(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        let mut guard = self.inner.lock();
        while guard.lifecycle == ExecutorServiceLifecycle::Running || occupied(&guard) != 0 {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return false;
            }
            self.terminated.wait_for(&mut guard, remaining);
        }
        true
    }

    /// Wakes termination waiters after the final accepted task reaches a
    /// terminal state.
    fn notify_if_terminated_locked(&self, inner: &RayonExecutorServiceInner) {
        if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(inner) == 0 {
            #[cfg(feature = "async-wait")]
            self.termination_tx.send_replace(true);
            self.terminated.notify_all();
        }
    }
}

/// Returns the number of accepted tasks that have not reached a terminal state.
fn occupied(inner: &RayonExecutorServiceInner) -> usize {
    inner.queue.len() + inner.running + inner.cancelling.len()
}
