// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::fmt;

use qubit_executor::service::SubmissionError;

/// A callable rejected by [`RayonExecutorService::try_submit_callable`].
///
/// The original callable is retained so it can be retried or handled by the
/// caller. Formatting this value never formats the callable or its captures.
///
/// # Type Parameters
///
/// * `T` - The rejected callable retained for the caller.
///
/// [`RayonExecutorService::try_submit_callable`]: crate::RayonExecutorService::try_submit_callable
pub struct RejectedTask<T> {
    /// Why the service could not accept the task.
    reason: SubmissionError,
    /// The original callable, which has not been run or dropped.
    task: T,
}

impl<T> RejectedTask<T> {
    /// Creates a rejected task. This constructor is intended for crate use.
    ///
    /// # Parameters
    ///
    /// * `reason` - The admission failure reason.
    /// * `task` - The callable returned to its owner.
    pub(crate) const fn new(reason: SubmissionError, task: T) -> Self {
        Self { reason, task }
    }

    /// Returns the reason the service rejected the task.
    ///
    /// # Returns
    ///
    /// The submission failure reason.
    pub const fn reason(&self) -> &SubmissionError {
        &self.reason
    }

    /// Returns the rejection reason and the original callable.
    ///
    /// # Returns
    ///
    /// The rejection reason and the same callable passed to the submission
    /// method.
    pub fn into_parts(self) -> (SubmissionError, T) {
        (self.reason, self.task)
    }
}

impl<T> fmt::Debug for RejectedTask<T> {
    /// Formats only the rejection reason, without inspecting the callable.
    ///
    /// # Parameters
    ///
    /// * `f` - Formatter receiving the displayed structure.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RejectedTask").field("reason", &self.reason).finish()
    }
}

impl<T> fmt::Display for RejectedTask<T> {
    /// Formats the rejection reason as a human-readable message.
    ///
    /// # Parameters
    ///
    /// * `f` - Formatter receiving the displayed message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task rejected: {}", self.reason)
    }
}

impl<T> std::error::Error for RejectedTask<T> {}
