// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use crate::queued_job::QueuedJob;

/// Result of claiming a task for an out-of-lock cancellation operation.
pub(crate) enum CancelDisposition {
    /// Transfers the queued job to the caller, which must cancel it outside
    /// the service-state lock.
    Owned(Box<dyn QueuedJob>),
    /// Indicates that another cancellation path already owns the job.
    InProgress,
    /// Indicates that the task is running or has already reached a terminal
    /// state.
    Absent,
}
