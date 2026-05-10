/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{future::IntoFuture, sync::Arc};

use qubit_executor::{
    CancelResult, TaskHandleFuture, TaskResult, TaskResultHandle, TaskStatus, TrackedTask,
    TrackedTaskHandle, TryGet,
};

use crate::{
    pending_cancel::PendingCancel, rayon_executor_service_state::RayonExecutorServiceState,
};

/// Tracked handle returned by [`crate::RayonExecutorService`] for accepted tasks.
///
/// This handle supports blocking [`Self::get`], asynchronous `.await`, status
/// inspection, and best-effort cancellation before a Rayon worker starts the
/// task.
pub struct RayonTaskHandle<R, E> {
    /// Shared task result and status observed through blocking and async APIs.
    inner: TrackedTask<R, E>,
    /// Stable identifier assigned by the owning executor service.
    task_id: usize,
    /// Shared service state used to keep cancellation counters consistent.
    state: Arc<RayonExecutorServiceState>,
    /// Cancellation hook that wins only before task start.
    cancel: PendingCancel,
}

impl<R, E> RayonTaskHandle<R, E> {
    /// Creates a Rayon task handle from a tracked task and cancel hook.
    ///
    /// # Parameters
    ///
    /// * `inner` - Tracked task used for result and status observation.
    /// * `task_id` - Stable identifier assigned to the accepted task.
    /// * `state` - Shared service state that owns lifecycle counters.
    /// * `cancel` - Cancellation hook that may cancel the task before start.
    ///
    /// # Returns
    ///
    /// A tracked handle for the accepted Rayon task.
    pub(crate) fn new(
        inner: TrackedTask<R, E>,
        task_id: usize,
        state: Arc<RayonExecutorServiceState>,
        cancel: PendingCancel,
    ) -> Self {
        Self {
            inner,
            task_id,
            state,
            cancel,
        }
    }

    /// Waits for the task to finish and returns its final result.
    ///
    /// # Returns
    ///
    /// The final task result reported through the underlying tracked task.
    #[inline]
    pub fn get(self) -> TaskResult<R, E>
    where
        R: Send,
        E: Send,
    {
        self.inner.get()
    }

    /// Attempts to retrieve the final result without blocking.
    ///
    /// # Returns
    ///
    /// A ready result or the pending Rayon task handle.
    #[inline]
    pub fn try_get(self) -> TryGet<Self, R, E>
    where
        R: Send,
        E: Send,
    {
        <Self as TaskResultHandle<R, E>>::try_get(self)
    }

    /// Attempts to cancel the task before any Rayon worker starts it.
    ///
    /// # Returns
    ///
    /// The observed cancellation outcome.
    #[inline]
    pub fn cancel(&self) -> CancelResult
    where
        R: Send,
        E: Send,
    {
        <Self as TrackedTaskHandle<R, E>>::cancel(self)
    }

    /// Returns whether the task has reported completion.
    ///
    /// # Returns
    ///
    /// `true` after the task has finished or has been cancelled.
    #[inline]
    pub fn is_done(&self) -> bool
    where
        R: Send,
        E: Send,
    {
        <Self as TaskResultHandle<R, E>>::is_done(self)
    }

    /// Returns the currently observed task status.
    ///
    /// # Returns
    ///
    /// The task's pending, running, or terminal status.
    #[inline]
    pub fn status(&self) -> TaskStatus {
        self.inner.status()
    }
}

impl<R, E> TaskResultHandle<R, E> for RayonTaskHandle<R, E>
where
    R: Send,
    E: Send,
{
    /// Returns whether the inner tracked task is done.
    #[inline]
    fn is_done(&self) -> bool {
        self.inner.is_done()
    }

    /// Blocks until the inner tracked task yields a final result.
    #[inline]
    fn get(self) -> TaskResult<R, E> {
        self.inner.get()
    }

    /// Attempts to retrieve the inner tracked task result without blocking.
    #[inline]
    fn try_get(self) -> TryGet<Self, R, E> {
        let Self {
            inner,
            task_id,
            state,
            cancel,
        } = self;
        match inner.try_get() {
            TryGet::Ready(result) => TryGet::Ready(result),
            TryGet::Pending(inner) => TryGet::Pending(Self {
                inner,
                task_id,
                state,
                cancel,
            }),
        }
    }
}

impl<R, E> TrackedTaskHandle<R, E> for RayonTaskHandle<R, E>
where
    R: Send,
    E: Send,
{
    /// Returns the currently observed task status.
    #[inline]
    fn status(&self) -> TaskStatus {
        self.inner.status()
    }

    /// Cancels the task through the owning service state.
    #[inline]
    fn cancel(&self) -> CancelResult {
        if self.state.cancel_pending_task(self.task_id, &self.cancel) {
            return CancelResult::Cancelled;
        }
        match self.status() {
            TaskStatus::Pending => CancelResult::Unsupported,
            TaskStatus::Running => CancelResult::AlreadyRunning,
            _ => CancelResult::AlreadyFinished,
        }
    }
}

impl<R, E> IntoFuture for RayonTaskHandle<R, E> {
    type Output = TaskResult<R, E>;
    type IntoFuture = TaskHandleFuture<R, E>;

    /// Converts this handle into a future resolving to the task result.
    #[inline]
    fn into_future(self) -> Self::IntoFuture {
        self.inner.into_future()
    }
}
