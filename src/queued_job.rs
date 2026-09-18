// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

/// Type-erased work retained by the executor's bounded pending queue.
///
/// Implementations publish acceptance, execute their user work, or cancel it
/// after the service removes them from the queue. Each terminal operation
/// consumes the job to release every captured task resource exactly once.
pub(crate) trait QueuedJob: Send {
    /// Publishes acceptance before a worker can observe this job.
    fn accept(&self);

    /// Executes the user work and publishes its terminal result.
    fn run(self: Box<Self>);

    /// Publishes cancellation without invoking the user work.
    fn cancel(self: Box<Self>);
}
