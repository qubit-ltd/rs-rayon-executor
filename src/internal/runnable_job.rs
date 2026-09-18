// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::marker::PhantomData;

use qubit_executor::task::spi::TaskRunner;
use qubit_function::Runnable;

use crate::queued_job::QueuedJob;

/// Queued fire-and-forget runnable.
pub(crate) struct RunnableJob<T, E> {
    /// User task executed after a Rayon worker claims this job.
    task: T,
    /// Preserves the runnable error type without storing a value of it.
    error: PhantomData<fn() -> E>,
}

impl<T, E> RunnableJob<T, E> {
    /// Stores a detached runnable until a worker handles it.
    pub(crate) const fn new(task: T) -> Self {
        Self {
            task,
            error: PhantomData,
        }
    }
}

impl<T, E> QueuedJob for RunnableJob<T, E>
where
    T: Runnable<E> + Send + 'static,
    E: Send + 'static,
{
    /// Does nothing because detached runnables have no result endpoint.
    fn accept(&self) {}

    /// Runs the detached task with the standard panic conversion.
    fn run(self: Box<Self>) {
        let mut task = self.task;
        TaskRunner::new(move || task.run()).run_detached::<(), E>();
    }

    /// Drops the runnable without executing it.
    fn cancel(self: Box<Self>) {}
}
