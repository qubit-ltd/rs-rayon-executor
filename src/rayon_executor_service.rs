/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{future::Future, pin::Pin, sync::Arc};

use qubit_function::{Callable, Runnable};
use rayon::ThreadPool as RayonThreadPool;

use qubit_executor::{TaskCompletionPair, TaskHandle, TaskRunner};

use qubit_executor::service::{
    ExecutorService, ExecutorServiceLifecycle, RejectedExecution, StopReport,
};

use crate::{
    pending_cancel::PendingCancel,
    rayon_executor_service_build_error::RayonExecutorServiceBuildError,
    rayon_executor_service_builder::RayonExecutorServiceBuilder,
    rayon_executor_service_state::RayonExecutorServiceState, rayon_task_handle::RayonTaskHandle,
};

/// Rayon-backed executor service for CPU-bound synchronous tasks.
///
/// Accepted tasks are executed on a dedicated Rayon thread pool. The service
/// preserves the crate's `ExecutorService` lifecycle semantics and task-handle
/// APIs while delegating scheduling to Rayon.
#[derive(Clone)]
pub struct RayonExecutorService {
    /// Rayon thread pool used to execute accepted tasks.
    pub(crate) pool: Arc<RayonThreadPool>,
    /// Shared lifecycle and cancellation state.
    pub(crate) state: Arc<RayonExecutorServiceState>,
}

impl RayonExecutorService {
    /// Creates a Rayon executor service with default builder settings.
    ///
    /// # Returns
    ///
    /// `Ok(RayonExecutorService)` if the default Rayon thread pool can be
    /// built.
    ///
    /// # Errors
    ///
    /// Returns [`RayonExecutorServiceBuildError`] if the default builder
    /// configuration is rejected.
    #[inline]
    pub fn new() -> Result<Self, RayonExecutorServiceBuildError> {
        Self::builder().build()
    }

    /// Creates a builder for configuring a Rayon executor service.
    ///
    /// # Returns
    ///
    /// A builder configured with CPU-parallelism defaults.
    #[inline]
    pub fn builder() -> RayonExecutorServiceBuilder {
        RayonExecutorServiceBuilder::default()
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

    type Termination<'a>
        = Pin<Box<dyn Future<Output = ()> + Send + 'a>>
    where
        Self: 'a;

    /// Accepts a runnable and schedules it on the Rayon thread pool.
    fn submit<T, E>(&self, task: T) -> Result<(), RejectedExecution>
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        let submission_guard = self.state.lock_submission();
        if self.state.is_not_running() {
            return Err(RejectedExecution::Shutdown);
        }
        let task_id = self.state.next_task_id();
        self.state.on_task_accepted();
        let cancel: PendingCancel = Arc::new(|| true);
        self.state
            .register_pending_task(task_id, Arc::clone(&cancel));
        drop(submission_guard);

        let state_for_run = Arc::clone(&self.state);
        self.pool.spawn_fifo(move || {
            if !state_for_run.start_pending_task(task_id, || true) {
                return;
            }
            let mut task = task;
            let _ignored = TaskRunner::new(move || task.run()).call::<(), E>();
            state_for_run.on_task_completed();
        });
        Ok(())
    }

    /// Accepts a callable and schedules it on the Rayon thread pool.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable to execute on a Rayon worker.
    ///
    /// # Returns
    ///
    /// A [`RayonTaskHandle`] for the accepted task.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::Shutdown`] if shutdown has already been
    /// requested before the task is accepted.
    fn submit_callable<C, R, E>(
        &self,
        task: C,
    ) -> Result<Self::ResultHandle<R, E>, RejectedExecution>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let submission_guard = self.state.lock_submission();
        if self.state.is_not_running() {
            return Err(RejectedExecution::Shutdown);
        }
        let task_id = self.state.next_task_id();
        self.state.on_task_accepted();
        let (handle, completion) = TaskCompletionPair::new().into_parts();
        let completion_for_cancel = completion.clone();
        let cancel: PendingCancel = Arc::new(move || completion_for_cancel.cancel());
        self.state
            .register_pending_task(task_id, Arc::clone(&cancel));
        drop(submission_guard);

        let completion_for_run = completion;
        let state_for_run = Arc::clone(&self.state);
        self.pool.spawn_fifo(move || {
            if !state_for_run.start_pending_task(task_id, || completion_for_run.start()) {
                return;
            }
            completion_for_run.complete(TaskRunner::new(task).call());
            state_for_run.on_task_completed();
        });
        Ok(handle)
    }

    /// Accepts a callable and schedules it with a tracked handle.
    fn submit_tracked_callable<C, R, E>(
        &self,
        task: C,
    ) -> Result<Self::TrackedHandle<R, E>, RejectedExecution>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let submission_guard = self.state.lock_submission();
        if self.state.is_not_running() {
            return Err(RejectedExecution::Shutdown);
        }
        let task_id = self.state.next_task_id();
        self.state.on_task_accepted();
        let (handle, completion) = TaskCompletionPair::new().into_tracked_parts();
        let completion_for_cancel = completion.clone();
        let cancel: PendingCancel = Arc::new(move || completion_for_cancel.cancel());
        self.state
            .register_pending_task(task_id, Arc::clone(&cancel));
        drop(submission_guard);

        let completion_for_run = completion;
        let state_for_run = Arc::clone(&self.state);
        self.pool.spawn_fifo(move || {
            if !state_for_run.start_pending_task(task_id, || completion_for_run.start()) {
                return;
            }
            completion_for_run.complete(TaskRunner::new(task).call());
            state_for_run.on_task_completed();
        });
        Ok(RayonTaskHandle::new(
            handle,
            task_id,
            Arc::clone(&self.state),
            cancel,
        ))
    }

    /// Stops accepting new tasks.
    ///
    /// Already accepted Rayon tasks are allowed to finish normally.
    fn shutdown(&self) {
        let _guard = self.state.lock_submission();
        self.state.shutdown();
        self.state.notify_if_terminated();
    }

    /// Stops accepting new tasks and cancels tasks that have not started yet.
    ///
    /// Running Rayon tasks cannot be preempted. Cancellation therefore applies
    /// only to tasks that are still pending when the cancellation hook wins the
    /// race against task start.
    ///
    /// # Returns
    ///
    /// A count-based report describing the pending and running work observed at
    /// the time of the shutdown request, plus the number of pending tasks for
    /// which cancellation succeeded.
    fn stop(&self) -> StopReport {
        let _guard = self.state.lock_submission();
        self.state.stop();
        let (queued, running, pending) = self.state.drain_pending_tasks_for_shutdown();
        drop(_guard);

        let cancelled = self.state.cancel_drained_pending_tasks(pending);
        StopReport::new(queued, running, cancelled)
    }

    /// Returns the current lifecycle state.
    fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.state.lifecycle()
    }

    /// Returns whether shutdown has been requested.
    fn is_not_running(&self) -> bool {
        self.state.is_not_running()
    }

    /// Returns whether shutdown was requested and no accepted tasks remain.
    fn is_terminated(&self) -> bool {
        self.lifecycle() == ExecutorServiceLifecycle::Terminated
    }

    /// Waits until the service has terminated.
    ///
    /// # Returns
    ///
    /// A future that resolves after shutdown has been requested and all
    /// accepted Rayon tasks have completed or been cancelled before start.
    fn await_termination(&self) -> Self::Termination<'_> {
        Box::pin(async move {
            loop {
                let notified = self.state.notified();
                if self.is_terminated() {
                    return;
                }
                notified.await;
            }
        })
    }
}
