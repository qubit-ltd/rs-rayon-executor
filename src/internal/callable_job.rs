// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use qubit_executor::task::spi::TaskRunner;
use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;

use crate::queued_job::QueuedJob;

/// Queued callable and the completion slot used to publish its result.
///
/// # Type Parameters
///
/// * `C` - Callable executed by a Rayon worker.
/// * `R` - Successful callable result.
/// * `E` - Callable error.
pub(crate) struct CallableJob<C, R, E> {
    /// User callable executed after the Rayon worker claims this job.
    task: C,
    /// Result endpoint observed by the callable's returned task handle.
    slot: TaskSlot<R, E>,
}

impl<C, R, E> CallableJob<C, R, E> {
    /// Recovers a callable from a job that was never accepted.
    #[expect(
        clippy::boxed_local,
        reason = "the rejected typed Box owns the result slot that must be released before returning its callable"
    )]
    pub(crate) fn into_task(self: Box<Self>) -> C {
        let Self { task, slot } = *self;
        drop(slot);
        task
    }

    /// Stores a callable and its completion slot until a worker handles it.
    ///
    /// # Type Parameters
    ///
    /// * `C` - Callable executed by a Rayon worker.
    /// * `R` - Successful callable result.
    /// * `E` - Callable error.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable to queue.
    /// * `slot` - Endpoint for publishing the callable result.
    #[must_use]
    #[inline]
    pub(crate) const fn new(task: C, slot: TaskSlot<R, E>) -> Self {
        Self { task, slot }
    }
}

impl<C, R, E> QueuedJob for CallableJob<C, R, E>
where
    C: Callable<R, E> + Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    /// Marks the result endpoint accepted before task execution can begin.
    fn accept(&self) {
        self.slot.accept();
    }

    /// Runs the callable through the standard task runner.
    ///
    /// The task runner protects callable invocation and destruction, converts
    /// panics into the task result, and safely discards panic payloads. A
    /// second panic during unwinding can still abort the process.
    fn run(self: Box<Self>) {
        let Self { task, slot } = *self;
        let _ignored = TaskRunner::new(task).run(slot);
    }

    /// Marks the endpoint cancelled and drops the unstarted callable.
    fn cancel(self: Box<Self>) {
        let Self { task, slot } = *self;
        let _ignored = slot.cancel_unstarted();
        drop(task);
    }
}
