// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
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

use crate::queued_job::QueuedJob;

/// Result of claiming a task for cancellation.
pub(crate) enum CancelDisposition {
    /// The caller owns the queued job and must cancel it outside the lock.
    Owned(Box<dyn QueuedJob>),
    /// Another cancellation path already owns the job.
    InProgress,
    /// The job is running or has reached a terminal state.
    Absent,
}

type OwnedQueuedJobs = Vec<(usize, Box<dyn QueuedJob>)>;

struct Inner {
    lifecycle: ExecutorServiceLifecycle,
    task_capacity: usize,
    num_threads: usize,
    next_task_id: usize,
    running: usize,
    scheduled: usize,
    queue: BTreeMap<usize, Box<dyn QueuedJob>>,
    cancelling: BTreeSet<usize>,
}

/// Shared state for the Rayon executor service.
pub(crate) struct RayonExecutorServiceState {
    inner: Mutex<Inner>,
    terminated: Condvar,
}

/// A reserved task admission and the number of Rayon closures to dispatch.
pub(crate) struct Admission {
    /// Stable task identifier assigned to the accepted task.
    pub(crate) task_id: usize,
    /// Number of worker closures the caller must dispatch.
    pub(crate) dispatch_count: usize,
}

impl RayonExecutorServiceState {
    /// Creates bounded executor state.
    pub(crate) fn new(task_capacity: usize, num_threads: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
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
        })
    }

    /// Accepts a queued job if lifecycle and capacity permit it.
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
    pub(crate) fn finish_cancelling(&self, task_id: usize) {
        let mut inner = self.inner.lock();
        inner.cancelling.remove(&task_id);
        self.notify_if_terminated_locked(&inner);
        self.terminated.notify_all();
    }

    /// Changes the lifecycle to graceful shutdown.
    pub(crate) fn shutdown(&self) {
        let mut inner = self.inner.lock();
        if inner.lifecycle == ExecutorServiceLifecycle::Running {
            inner.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        self.notify_if_terminated_locked(&inner);
    }

    /// Stops admission and takes all still queued jobs for cancellation.
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
        (StopReport::new(queued, running, owned.len()), owned)
    }

    /// Returns the current lifecycle, deriving termination from task counters.
    pub(crate) fn lifecycle(&self) -> ExecutorServiceLifecycle {
        let inner = self.inner.lock();
        if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(&inner) == 0 {
            ExecutorServiceLifecycle::Terminated
        } else {
            inner.lifecycle
        }
    }

    /// Returns whether new submissions are rejected.
    pub(crate) fn is_not_running(&self) -> bool {
        self.inner.lock().lifecycle != ExecutorServiceLifecycle::Running
    }

    /// Waits until lifecycle termination.
    pub(crate) fn wait_for_termination(&self) {
        let mut guard = self.inner.lock();
        while guard.lifecycle == ExecutorServiceLifecycle::Running || occupied(&guard) != 0 {
            self.terminated.wait(&mut guard);
        }
    }

    /// Waits up to the supplied duration for lifecycle termination.
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

    fn notify_if_terminated_locked(&self, inner: &Inner) {
        if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(inner) == 0 {
            self.terminated.notify_all();
        }
    }
}

fn occupied(inner: &Inner) -> usize {
    inner.queue.len() + inner.running + inner.cancelling.len()
}
