// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(feature = "async-wait")]
use std::panic::AssertUnwindSafe;
#[cfg(feature = "async-wait")]
use std::panic::catch_unwind;
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

use crate::RayonExecutorServiceStats;
use crate::internal::Admission;
use crate::internal::CancelDisposition;
use crate::internal::RayonExecutorServiceInner;
#[cfg(feature = "async-wait")]
use crate::internal::discard_panic_payload;
use crate::queued_job::QueuedJob;

/// Collection of queued jobs that must be cancelled after releasing the lock.
type OwnedQueuedJobs = Vec<(u64, Box<dyn QueuedJob>)>;

/// Shared state for the Rayon executor service.
pub(crate) struct RayonExecutorServiceState {
    /// Mutex protecting lifecycle counters and queued jobs.
    inner: Mutex<RayonExecutorServiceInner>,
    /// Condition variable notified when accepted work reaches termination.
    terminated: Condvar,
    /// Monotonic notification for asynchronous termination waiters.
    #[cfg(feature = "async-wait")]
    termination_tx: watch::Sender<bool>,
    /// Generation counter notifying asynchronous capacity waiters.
    #[cfg(feature = "async-wait")]
    capacity_tx: watch::Sender<u64>,
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
                next_task_id: Some(0),
                running: 0,
                running_tasks: BTreeSet::new(),
                scheduled: 0,
                queue: BTreeMap::new(),
                cancelling: BTreeSet::new(),
            }),
            terminated: Condvar::new(),
            #[cfg(feature = "async-wait")]
            termination_tx: watch::channel(false).0,
            #[cfg(feature = "async-wait")]
            capacity_tx: watch::channel(0).0,
        })
    }

    /// Subscribes before checking the termination predicate under the state
    /// lock, then repairs the watch value after releasing that lock if needed.
    ///
    /// # Returns
    ///
    /// A receiver that observes termination, including when the service had
    /// already terminated before the subscription was created.
    #[cfg(feature = "async-wait")]
    pub(crate) fn subscribe_termination(&self) -> watch::Receiver<bool> {
        // Subscribe before checking state. If termination races with this
        // check, either this receiver observes the publication or the check
        // below repairs a publication that happened before subscription.
        let receiver = self.termination_tx.subscribe();
        let terminated = {
            let inner = self.inner.lock();
            inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(&inner) == 0
        };
        if terminated {
            // Repair stale state after unlocking without waking existing
            // receivers when the terminal value is already published.
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
                self.termination_tx.send_if_modified(|current| {
                    if *current {
                        false
                    } else {
                        *current = true;
                        true
                    }
                });
            })) {
                discard_panic_payload(payload);
            }
        }
        receiver
    }

    /// Subscribes to state changes that may affect task admission.
    ///
    /// # Returns
    ///
    /// A receiver that changes when service capacity or lifecycle changes. A
    /// notification is only a hint; callers must retry admission.
    #[cfg(feature = "async-wait")]
    pub(crate) fn capacity_changes(&self) -> crate::CapacityChanges {
        let _inner = self.inner.lock();
        self.capacity_tx.subscribe().into()
    }

    /// Returns a consistent snapshot of queue, worker, and lifecycle state.
    ///
    /// # Returns
    ///
    /// The configured capacity and current counts sampled under the state lock.
    #[must_use]
    pub(crate) fn stats(&self) -> RayonExecutorServiceStats {
        let inner = self.inner.lock();
        let lifecycle = if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(&inner) == 0 {
            ExecutorServiceLifecycle::Terminated
        } else {
            inner.lifecycle
        };
        RayonExecutorServiceStats {
            lifecycle,
            task_capacity: inner.task_capacity,
            queued: inner.queue.len(),
            running: inner.running,
            cancelling: inner.cancelling.len(),
        }
    }

    /// Returns the current lifecycle, deriving termination from task counters.
    ///
    /// # Returns
    ///
    /// `Terminated` after shutdown or stop when no accepted task remains;
    /// otherwise the current nonterminal lifecycle.
    #[must_use = "the lifecycle must be checked"]
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
    /// `true` once graceful shutdown, stop, or task identifier exhaustion has
    /// begun.
    #[must_use]
    pub(crate) fn is_not_running(&self) -> bool {
        self.inner.lock().lifecycle != ExecutorServiceLifecycle::Running
    }

    /// Accepts a queued job if lifecycle and capacity permit it.
    ///
    /// The job is marked accepted while the state mutex is held, preventing a
    /// worker from observing an unaccepted result endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] when not running or when all
    /// unique task identifiers have been used, and
    /// [`SubmissionError::Saturated`] when the accepted-task limit is reached.
    ///
    /// # Parameters
    ///
    /// * `job` - Owned job to accept into the queue.
    ///
    /// # Returns
    ///
    /// An [`Admission`] containing the assigned task identifier and number of
    /// worker closures to dispatch when accepted, or the error and original
    /// job when admission is rejected.
    ///
    /// # Type Parameters
    ///
    /// * `J` - Concrete queued job type, retained if admission fails.
    pub(crate) fn admit<J: QueuedJob + 'static>(&self, job: Box<J>) -> Result<Admission, (SubmissionError, Box<J>)> {
        let mut inner = self.inner.lock();
        if inner.lifecycle != ExecutorServiceLifecycle::Running {
            return Err((SubmissionError::Shutdown, job));
        }
        if inner.next_task_id.is_none() {
            inner.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
            let terminated = self.notify_if_terminated_locked(&inner);
            drop(inner);
            self.notify_async_changes(true, terminated);
            return Err((SubmissionError::Shutdown, job));
        }
        if occupied(&inner) >= inner.task_capacity {
            return Err((SubmissionError::Saturated, job));
        }
        let task_id = inner.next_task_id.expect("exhaustion handled before capacity check");
        inner.next_task_id = task_id.checked_add(1);
        job.accept();
        let job: Box<dyn QueuedJob> = job;
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
    pub(crate) fn take_next(&self) -> Option<(u64, Box<dyn QueuedJob>)> {
        let mut inner = self.inner.lock();
        let Some((task_id, job)) = inner.queue.pop_first() else {
            inner.scheduled = inner.scheduled.saturating_sub(1);
            let terminated = self.notify_if_terminated_locked(&inner);
            drop(inner);
            self.notify_async_changes(false, terminated);
            return None;
        };
        inner.running += 1;
        inner.running_tasks.insert(task_id);
        Some((task_id, job))
    }

    /// Returns whether a worker currently owns the specified task.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Identifier of the task to inspect.
    ///
    /// # Returns
    ///
    /// `true` while a worker owns the task, and `false` otherwise.
    pub(crate) fn is_running_task(&self, task_id: u64) -> bool {
        self.inner.lock().running_tasks.contains(&task_id)
    }

    /// Finishes one running task and returns whether a successor closure is
    /// needed.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Identifier of the task whose worker ownership is released.
    ///
    /// # Returns
    ///
    /// `true` when queued work needs the same worker reservation to run next.
    #[must_use]
    pub(crate) fn finish_running(&self, task_id: u64) -> bool {
        let mut inner = self.inner.lock();
        inner.running_tasks.remove(&task_id);
        inner.running = inner.running.saturating_sub(1);
        let dispatch = !inner.queue.is_empty();
        if !dispatch {
            inner.scheduled = inner.scheduled.saturating_sub(1);
        }
        let terminated = self.notify_if_terminated_locked(&inner);
        drop(inner);
        self.notify_async_changes(true, terminated);
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
    #[must_use]
    pub(crate) fn cancel_pending_task(&self, task_id: u64) -> CancelDisposition {
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
    ///
    /// # Parameters
    ///
    /// * `task_id` - Identifier of the job whose cleanup has completed.
    pub(crate) fn finish_cancelling(&self, task_id: u64) {
        let mut inner = self.inner.lock();
        inner.cancelling.remove(&task_id);
        let terminated = self.notify_if_terminated_locked(&inner);
        self.terminated.notify_all();
        drop(inner);
        self.notify_async_changes(true, terminated);
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
        let terminated = self.notify_if_terminated_locked(&inner);
        drop(inner);
        self.notify_async_changes(true, terminated);
    }

    /// Stops admission and takes all still queued jobs for cancellation.
    ///
    /// # Returns
    ///
    /// A stop report and jobs that the caller must cancel outside this mutex to
    /// avoid executing user cleanup while holding service state.
    #[must_use]
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
        let terminated = self.notify_if_terminated_locked(&inner);
        drop(inner);
        self.notify_async_changes(true, terminated);
        (StopReport::new(queued, running, owned.len()), owned)
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
    #[must_use]
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

    /// Publishes asynchronous state changes after the service lock is released.
    ///
    /// Watch publication may invoke a receiver's waker synchronously. Keeping
    /// it outside `inner` allows a waker to safely call back into the service.
    ///
    /// # Parameters
    ///
    /// * `capacity_changed` - Whether to advance the capacity generation.
    /// * `terminated` - Whether to publish the terminal state.
    #[cfg(feature = "async-wait")]
    fn notify_async_changes(&self, capacity_changed: bool, terminated: bool) {
        if capacity_changed
            && let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
                self.capacity_tx
                    .send_modify(|generation| *generation = generation.wrapping_add(1));
            }))
        {
            discard_panic_payload(payload);
        }
        if terminated
            && let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
                self.termination_tx.send_if_modified(|current| {
                    if *current {
                        false
                    } else {
                        *current = true;
                        true
                    }
                });
            }))
        {
            discard_panic_payload(payload);
        }
    }

    /// Does nothing when asynchronous state notifications are disabled.
    ///
    /// # Parameters
    ///
    /// * `capacity_changed` - Whether capacity changed.
    /// * `terminated` - Whether the service terminated.
    #[cfg(not(feature = "async-wait"))]
    fn notify_async_changes(&self, _capacity_changed: bool, _terminated: bool) {}

    /// Wakes termination waiters after the final accepted task is terminal.
    ///
    /// # Parameters
    ///
    /// * `inner` - Locked service state used to determine whether all work
    ///   ended.
    ///
    /// # Returns
    ///
    /// `true` when termination is observed and waiters are notified; `false`
    /// while the service is running or accepted tasks remain.
    fn notify_if_terminated_locked(&self, inner: &RayonExecutorServiceInner) -> bool {
        if inner.lifecycle != ExecutorServiceLifecycle::Running && occupied(inner) == 0 {
            self.terminated.notify_all();
            true
        } else {
            false
        }
    }
}

/// Returns the number of accepted tasks that have not reached a terminal state.
///
/// # Parameters
///
/// * `inner` - Service state containing queued, running, and cancelling tasks.
///
/// # Returns
///
/// The combined number of queued, running, and cancelling accepted tasks.
fn occupied(inner: &RayonExecutorServiceInner) -> usize {
    inner.queue.len() + inner.running + inner.cancelling.len()
}

#[cfg(test)]
mod task_id_tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::Admission;
    use super::CancelDisposition;
    use super::ExecutorServiceLifecycle;
    use super::QueuedJob;
    use super::RayonExecutorServiceState;
    use super::SubmissionError;

    #[derive(Debug)]
    struct TestJob(Option<Arc<AtomicUsize>>);

    impl Drop for TestJob {
        fn drop(&mut self) {
            if let Some(drops) = &self.0 {
                drops.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl QueuedJob for TestJob {
        fn accept(&self) {}

        fn run(self: Box<Self>) {}

        fn cancel(self: Box<Self>) {}
    }

    fn job() -> Box<TestJob> {
        Box::new(TestJob(None))
    }

    fn admit(state: &RayonExecutorServiceState, job: Box<TestJob>) -> Admission {
        match state.admit(job) {
            Ok(admission) => admission,
            Err(_) => panic!("test job should be accepted"),
        }
    }

    fn reject(state: &RayonExecutorServiceState, job: Box<TestJob>) -> (SubmissionError, Box<TestJob>) {
        match state.admit(job) {
            Err(rejected) => rejected,
            Ok(_) => panic!("test job should be rejected"),
        }
    }

    #[test]
    fn test_task_id_crosses_u32_boundary_without_reusing_ids() {
        let state = RayonExecutorServiceState::new(2, 1);
        state.inner.lock().next_task_id = Some(u64::from(u32::MAX) - 1);

        let first = admit(&state, job());
        let second = admit(&state, job());

        assert_eq!(first.task_id, u64::from(u32::MAX) - 1);
        assert_eq!(second.task_id, u64::from(u32::MAX));
        assert_ne!(first.task_id, second.task_id);
        assert_eq!(state.inner.lock().next_task_id, Some(u64::from(u32::MAX) + 1));
    }

    #[test]
    fn test_task_id_u64_max_is_admitted_once_then_shutdown_returns_job() {
        let state = RayonExecutorServiceState::new(1, 1);
        let drops = Arc::new(AtomicUsize::new(0));
        state.inner.lock().next_task_id = Some(u64::MAX);

        let last = admit(&state, Box::new(TestJob(None)));
        assert_eq!(last.task_id, u64::MAX);
        assert_eq!(state.inner.lock().next_task_id, None);

        let rejected = reject(&state, Box::new(TestJob(Some(Arc::clone(&drops)))));
        assert_eq!(rejected.0, SubmissionError::Shutdown);
        assert_eq!(state.lifecycle(), ExecutorServiceLifecycle::ShuttingDown);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "rejected job remains owned by caller");
        drop(rejected.1);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "caller drop destroys rejected job once"
        );

        let (_, accepted) = state.take_next().expect("last accepted job remains queued");
        accepted.run();
        assert!(!state.finish_running(last.task_id));
        assert_eq!(state.lifecycle(), ExecutorServiceLifecycle::Terminated);
    }

    #[test]
    fn test_task_id_exhaustion_precedes_saturation_and_terminates_idle_service() {
        let full = RayonExecutorServiceState::new(1, 1);
        admit(&full, job());
        full.inner.lock().next_task_id = None;
        let rejected = reject(&full, job());
        assert_eq!(rejected.0, SubmissionError::Shutdown);
        drop(rejected.1);
        assert_eq!(full.lifecycle(), ExecutorServiceLifecycle::ShuttingDown);

        let idle = RayonExecutorServiceState::new(1, 1);
        idle.inner.lock().next_task_id = None;
        let rejected = reject(&idle, job());
        assert_eq!(rejected.0, SubmissionError::Shutdown);
        drop(rejected.1);
        assert_eq!(idle.lifecycle(), ExecutorServiceLifecycle::Terminated);
        assert!(idle.wait_for_termination_timeout(Duration::ZERO));
    }

    #[test]
    fn test_task_id_cancellation_of_old_handle_does_not_remove_later_job() {
        let state = RayonExecutorServiceState::new(2, 1);
        state.inner.lock().next_task_id = Some(41);
        let old = admit(&state, job());
        let CancelDisposition::Owned(removed) = state.cancel_pending_task(old.task_id) else {
            panic!("old pending job should be removable");
        };
        drop(removed);
        state.finish_cancelling(old.task_id);

        let new = admit(&state, job());
        assert_ne!(old.task_id, new.task_id);
        assert!(matches!(
            state.cancel_pending_task(old.task_id),
            CancelDisposition::Absent
        ));
        assert!(state.inner.lock().queue.contains_key(&new.task_id));
    }

    #[cfg(feature = "async-wait")]
    #[test]
    fn test_task_id_exhaustion_notifies_async_capacity_and_termination_waiters() {
        use std::task::Context;
        use std::task::Poll;
        use std::task::Wake;
        use std::task::Waker;

        struct Counter(AtomicUsize);

        impl Wake for Counter {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let state = RayonExecutorServiceState::new(1, 1);
        let accepted = admit(&state, job());
        let mut capacity = state.capacity_changes();
        let mut capacity_wait = Box::pin(capacity.changed());
        let mut termination = state.subscribe_termination();
        let mut termination_wait = Box::pin(termination.changed());
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut context = Context::from_waker(&waker);

        assert!(matches!(capacity_wait.as_mut().poll(&mut context), Poll::Pending));
        assert!(matches!(termination_wait.as_mut().poll(&mut context), Poll::Pending));
        state.inner.lock().next_task_id = None;
        let rejected = reject(&state, job());
        assert_eq!(rejected.0, SubmissionError::Shutdown);
        drop(rejected.1);

        assert!(
            counter.0.load(Ordering::SeqCst) >= 1,
            "capacity waiter should be woken on exhausted admission"
        );
        assert!(matches!(capacity_wait.as_mut().poll(&mut context), Poll::Ready(Ok(()))));
        assert!(matches!(termination_wait.as_mut().poll(&mut context), Poll::Pending));

        let (_, accepted_job) = state.take_next().expect("accepted work remains queued");
        accepted_job.run();
        assert!(!state.finish_running(accepted.task_id));
        assert!(
            counter.0.load(Ordering::SeqCst) >= 2,
            "termination waiter should be woken after accepted work completes"
        );
        assert!(matches!(
            termination_wait.as_mut().poll(&mut context),
            Poll::Ready(Ok(()))
        ));
    }
}
