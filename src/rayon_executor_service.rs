// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::sync::Arc;
use std::time::Duration;

use qubit_executor::TaskHandle;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
use qubit_executor::task::spi::TaskEndpointPair;
use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;
use qubit_function::Runnable;
use rayon::ThreadPool as RayonThreadPool;

use crate::internal::Admission;
use crate::internal::CallableJob;
use crate::internal::RunnableJob;
use crate::queued_job::QueuedJob;
use crate::rayon_executor_service_build_error::RayonExecutorServiceBuildError;
use crate::rayon_executor_service_builder::RayonExecutorServiceBuilder;
use crate::rayon_executor_service_state::RayonExecutorServiceState;
use crate::rayon_task_handle::RayonTaskHandle;

/// Rayon-backed executor service for bounded, CPU-bound synchronous tasks.
///
/// The service owns a dedicated Rayon pool. Accepted tasks are bounded by the
/// configured capacity; shutdown allows queued work to finish, while stop
/// cancels work that has not started.
///
/// # Examples
///
/// ```
/// use qubit_executor::service::ExecutorService;
/// use qubit_rayon_executor::RayonExecutorService;
///
/// let service = RayonExecutorService::builder().num_threads(1).build()?;
/// assert!(!service.is_terminated());
/// # Ok::<(), qubit_rayon_executor::RayonExecutorServiceBuildError>(())
/// ```
#[derive(Clone)]
pub struct RayonExecutorService {
    /// Rayon thread pool used to execute accepted tasks.
    pub(crate) pool: Arc<RayonThreadPool>,
    /// Shared bounded scheduling state.
    pub(crate) state: Arc<RayonExecutorServiceState>,
}

impl RayonExecutorService {
    /// Creates a Rayon executor service with the default builder settings.
    ///
    /// # Returns
    ///
    /// A service using the detected CPU parallelism and the default bounded
    /// task capacity.
    ///
    /// # Errors
    ///
    /// Returns [`RayonExecutorServiceBuildError`] when Rayon rejects the
    /// default thread-pool configuration.
    pub fn new() -> Result<Self, RayonExecutorServiceBuildError> {
        Self::builder().build()
    }

    /// Creates a builder for configuring a Rayon executor service.
    ///
    /// # Returns
    ///
    /// A builder initialized with CPU-parallelism defaults.
    #[must_use]
    pub fn builder() -> RayonExecutorServiceBuilder {
        RayonExecutorServiceBuilder::default()
    }

    /// Admits a callable after converting its endpoint pair into a handle and
    /// slot.
    ///
    /// The callable, result, error, and returned handle must be transferable to
    /// the Rayon worker as required by the supplied endpoint conversion.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown or stop, and
    /// [`SubmissionError::Saturated`] when the accepted-task capacity is full.
    fn submit_callable_with<C, R, E, H, F>(&self, task: C, split: F) -> Result<H, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
        F: FnOnce(TaskEndpointPair<R, E>) -> (H, TaskSlot<R, E>),
    {
        let (handle, slot) = split(TaskEndpointPair::new());
        let job: Box<dyn QueuedJob> = Box::new(CallableJob::new(task, slot));
        let admission = self.state.admit(job)?;
        self.dispatch(admission.dispatch_count);
        Ok(handle)
    }

    /// Admits a detached runnable and schedules it for Rayon execution.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown or stop, and
    /// [`SubmissionError::Saturated`] when the accepted-task capacity is full.
    fn submit_runnable<T, E>(&self, task: T) -> Result<(), SubmissionError>
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        let admission = self.state.admit(Box::new(RunnableJob::<T, E>::new(task)))?;
        self.dispatch(admission.dispatch_count);
        Ok(())
    }

    /// Schedules exactly `count` worker closures to drain admitted queued work.
    fn dispatch(&self, count: usize) {
        for _ in 0..count {
            let pool = Arc::clone(&self.pool);
            let state = Arc::clone(&self.state);
            self.pool.spawn_fifo(move || run_one(pool, state));
        }
    }
}

/// Runs one queued job and schedules a successor when queued work remains.
///
/// Panics from user tasks are caught so the service state can account for the
/// completed worker closure before Rayon executes another queued job.
fn run_one(pool: Arc<RayonThreadPool>, state: Arc<RayonExecutorServiceState>) {
    let Some(job) = state.take_next() else {
        return;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| job.run()));
    if state.finish_running() {
        let next_pool = Arc::clone(&pool);
        pool.spawn_fifo(move || run_one(next_pool, state));
    }
}

impl ExecutorService for RayonExecutorService {
    type ResultHandle<R, E>
        = TaskHandle<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    type TrackedHandle<R, E>
        = RayonTaskHandle<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    /// Accepts a runnable and schedules it on the Rayon thread pool.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown or stop, and
    /// [`SubmissionError::Saturated`] when the accepted-task capacity is full.
    fn submit<T, E>(&self, task: T) -> Result<(), SubmissionError>
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        self.submit_runnable(task)
    }

    /// Accepts a callable and returns a handle for its eventual result.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown or stop, and
    /// [`SubmissionError::Saturated`] when the accepted-task capacity is full.
    fn submit_callable<C, R, E>(&self, task: C) -> Result<Self::ResultHandle<R, E>, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        self.submit_callable_with(task, TaskEndpointPair::into_parts)
    }

    /// Accepts a callable and returns a tracked handle for its eventual result.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown or stop, and
    /// [`SubmissionError::Saturated`] when the accepted-task capacity is full.
    fn submit_tracked_callable<C, R, E>(&self, task: C) -> Result<Self::TrackedHandle<R, E>, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let (handle, slot) = TaskEndpointPair::into_tracked_parts(TaskEndpointPair::new());
        let job: Box<dyn QueuedJob> = Box::new(CallableJob::new(task, slot));
        let Admission {
            task_id,
            dispatch_count,
        } = self.state.admit(job)?;
        self.dispatch(dispatch_count);
        Ok(RayonTaskHandle::new(handle, task_id, Arc::clone(&self.state)))
    }

    /// Stops accepting new tasks while allowing accepted tasks to finish.
    ///
    /// This operation is idempotent and changes subsequent submission errors
    /// to [`SubmissionError::Shutdown`].
    fn shutdown(&self) {
        self.state.shutdown();
    }

    /// Stops accepting tasks and cancels queued tasks.
    ///
    /// Running CPU work is not forcibly interrupted. The returned report
    /// distinguishes running, queued, and cancelled tasks.
    fn stop(&self) -> StopReport {
        let (report, jobs) = self.state.stop();
        for (task_id, job) in jobs {
            let _ = catch_unwind(AssertUnwindSafe(|| job.cancel()));
            self.state.finish_cancelling(task_id);
        }
        report
    }

    /// Returns the current lifecycle state derived from the shared counters.
    fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.state.lifecycle()
    }

    /// Returns whether shutdown or stop has been requested.
    fn is_not_running(&self) -> bool {
        self.state.is_not_running()
    }

    /// Returns whether every accepted task has reached a terminal state after
    /// shutdown or stop.
    fn is_terminated(&self) -> bool {
        self.lifecycle() == ExecutorServiceLifecycle::Terminated
    }

    /// Blocks until all accepted tasks reach a terminal state.
    ///
    /// Calling this before shutdown or stop blocks until a lifecycle transition
    /// is requested and all accepted work subsequently finishes or cancels.
    fn wait_termination(&self) {
        self.state.wait_for_termination();
    }

    /// Waits at most `timeout` for all accepted tasks to terminate.
    ///
    /// # Parameters
    ///
    /// * `timeout` - Maximum blocking duration.
    ///
    /// # Returns
    ///
    /// `true` if termination is observed before the timeout; otherwise `false`.
    fn wait_termination_timeout(&self, timeout: Duration) -> bool {
        self.state.wait_for_termination_timeout(timeout)
    }
}
