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
pub(crate) struct CallableJob<C, R, E> {
    /// User callable executed after the Rayon worker claims this job.
    task: C,
    /// Result endpoint observed by the callable's returned task handle.
    slot: TaskSlot<R, E>,
}

impl<C, R, E> CallableJob<C, R, E> {
    /// Stores a callable and its completion slot until a worker handles it.
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
