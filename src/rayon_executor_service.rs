// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
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
use qubit_function::Callable;
use qubit_function::Runnable;
use rayon::ThreadPool as RayonThreadPool;

use crate::queued_job::CallableJob;
use crate::queued_job::QueuedJob;
use crate::queued_job::RunnableJob;
use crate::rayon_executor_service_build_error::RayonExecutorServiceBuildError;
use crate::rayon_executor_service_builder::RayonExecutorServiceBuilder;
use crate::rayon_executor_service_state::Admission;
use crate::rayon_executor_service_state::RayonExecutorServiceState;
use crate::rayon_task_handle::RayonTaskHandle;

/// Rayon-backed executor service for CPU-bound synchronous tasks.
#[derive(Clone)]
pub struct RayonExecutorService {
    /// Rayon thread pool used to execute accepted tasks.
    pub(crate) pool: Arc<RayonThreadPool>,
    /// Shared bounded scheduling state.
    pub(crate) state: Arc<RayonExecutorServiceState>,
}

impl RayonExecutorService {
    /// Creates a Rayon executor service with default builder settings.
    pub fn new() -> Result<Self, RayonExecutorServiceBuildError> {
        Self::builder().build()
    }

    /// Creates a builder for configuring a Rayon executor service.
    pub fn builder() -> RayonExecutorServiceBuilder {
        RayonExecutorServiceBuilder::default()
    }

    fn submit_callable_with<C, R, E, H, F>(&self, task: C, split: F) -> Result<H, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
        F: FnOnce(TaskEndpointPair<R, E>) -> (H, qubit_executor::task::spi::TaskSlot<R, E>),
    {
        let (handle, slot) = split(TaskEndpointPair::new());
        let job: Box<dyn QueuedJob> = Box::new(CallableJob::new(task, slot));
        let admission = self.state.admit(job)?;
        self.dispatch(admission.dispatch_count);
        Ok(handle)
    }

    fn submit_runnable<T, E>(&self, task: T) -> Result<(), SubmissionError>
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        let admission = self.state.admit(Box::new(RunnableJob::<T, E>::new(task)))?;
        self.dispatch(admission.dispatch_count);
        Ok(())
    }

    fn dispatch(&self, count: usize) {
        for _ in 0..count {
            let pool = Arc::clone(&self.pool);
            let state = Arc::clone(&self.state);
            self.pool.spawn_fifo(move || run_one(pool, state));
        }
    }
}

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
    fn submit<T, E>(&self, task: T) -> Result<(), SubmissionError>
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        self.submit_runnable(task)
    }

    /// Accepts a callable and returns its result handle.
    fn submit_callable<C, R, E>(&self, task: C) -> Result<Self::ResultHandle<R, E>, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        self.submit_callable_with(task, TaskEndpointPair::into_parts)
    }

    /// Accepts a callable and returns a tracked result handle.
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
    fn shutdown(&self) {
        self.state.shutdown();
    }

    /// Stops accepting tasks and cancels queued tasks.
    fn stop(&self) -> StopReport {
        let (report, jobs) = self.state.stop();
        for (task_id, job) in jobs {
            let _ = catch_unwind(AssertUnwindSafe(|| job.cancel()));
            self.state.finish_cancelling(task_id);
        }
        report
    }

    /// Returns the current lifecycle state.
    fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.state.lifecycle()
    }

    /// Returns whether shutdown or stop has been requested.
    fn is_not_running(&self) -> bool {
        self.state.is_not_running()
    }

    /// Returns whether the service has terminated.
    fn is_terminated(&self) -> bool {
        self.lifecycle() == ExecutorServiceLifecycle::Terminated
    }

    /// Blocks until all accepted tasks reach a terminal state.
    fn wait_termination(&self) {
        self.state.wait_for_termination();
    }

    /// Waits at most `timeout` for all accepted tasks to terminate.
    fn wait_termination_timeout(&self, timeout: Duration) -> bool {
        self.state.wait_for_termination_timeout(timeout)
    }
}
