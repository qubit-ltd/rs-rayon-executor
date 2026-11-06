// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use crate::queued_job::QueuedJob;

/// Result of claiming a task for an out-of-lock cancellation operation.
///
/// This records whether the caller owns queued work to cancel, another path is
/// already cleaning it up, or no queued work remains to claim.
pub(crate) enum CancelDisposition {
    /// Transfers the queued job to the caller, which must cancel it outside
    /// the service-state lock.
    Owned(
        /// Queued job transferred to the caller for cancellation.
        Box<dyn QueuedJob>,
    ),
    /// Indicates that another cancellation path already owns the job.
    InProgress,
    /// Indicates that the task is running or has already reached a terminal
    /// state.
    Absent,
}
