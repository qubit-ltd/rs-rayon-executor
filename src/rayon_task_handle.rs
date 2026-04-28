/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{
        Context,
        Poll,
    },
};

use qubit_executor::{
    TaskHandle,
    TaskResult,
};

use crate::{
    pending_cancel::PendingCancel,
    rayon_executor_service_state::RayonExecutorServiceState,
};

/// Handle returned by [`crate::RayonExecutorService`] for accepted tasks.
///
/// This handle supports blocking [`Self::get`], asynchronous `.await`, and
/// best-effort cancellation before a Rayon worker starts the task.
pub struct RayonTaskHandle<R, E> {
    /// Shared task result observed through blocking and async APIs.
    inner: TaskHandle<R, E>,
    /// Stable identifier assigned by the owning executor service.
    task_id: usize,
    /// Shared service state used to keep cancellation counters consistent.
    state: Arc<RayonExecutorServiceState>,
    /// Cancellation hook that wins only before task start.
    cancel: PendingCancel,
}

impl<R, E> RayonTaskHandle<R, E> {
    /// Creates a Rayon task handle from a shared task handle and cancel hook.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared task handle used for result observation.
    /// * `task_id` - Stable identifier assigned to the accepted task.
    /// * `state` - Shared service state that owns lifecycle counters.
    /// * `cancel` - Cancellation hook that may cancel the task before start.
    ///
    /// # Returns
    ///
    /// A handle for the accepted Rayon task.
    pub(crate) fn new(
        inner: TaskHandle<R, E>,
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
    /// The final task result reported through the underlying [`TaskHandle`].
    #[inline]
    pub fn get(self) -> TaskResult<R, E> {
        self.inner.get()
    }

    /// Attempts to cancel the task before any Rayon worker starts it.
    ///
    /// # Returns
    ///
    /// `true` if the task was cancelled before start, or `false` if it had
    /// already started or completed.
    #[inline]
    pub fn cancel(&self) -> bool {
        self.state.cancel_pending_task(self.task_id, &self.cancel)
    }

    /// Returns whether the task has reported completion.
    ///
    /// # Returns
    ///
    /// `true` after the task has finished or has been cancelled.
    #[inline]
    pub fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}

impl<R, E> Future for RayonTaskHandle<R, E> {
    type Output = TaskResult<R, E>;

    /// Polls the accepted Rayon task for completion.
    ///
    /// # Parameters
    ///
    /// * `cx` - Async task context used to register the current waker.
    ///
    /// # Returns
    ///
    /// `Poll::Ready` with the task result after completion, or
    /// `Poll::Pending` while the task is still running or queued.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll(cx)
    }
}
