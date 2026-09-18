// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
// =============================================================================
use std::marker::PhantomData;

use qubit_executor::task::spi::TaskRunner;
use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;
use qubit_function::Runnable;

/// Type-erased task retained by the executor's bounded pending queue.
pub(crate) trait QueuedJob: Send {
    /// Publishes acceptance before the job can be observed by a worker.
    fn accept(&self);

    /// Runs the task and publishes its result.
    fn run(self: Box<Self>);

    /// Cancels the task without running its user callable.
    fn cancel(self: Box<Self>);
}

/// Queued callable with a caller-facing completion slot.
pub(crate) struct CallableJob<C, R, E> {
    task: C,
    slot: TaskSlot<R, E>,
}

impl<C, R, E> CallableJob<C, R, E> {
    /// Creates a queued callable job.
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
    /// Marks the completion slot accepted.
    fn accept(&self) {
        self.slot.accept();
    }

    /// Runs the callable through the standard task runner.
    fn run(self: Box<Self>) {
        let Self { task, slot } = *self;
        let _ignored = TaskRunner::new(task).run(slot);
    }

    /// Publishes cancellation and releases the callable.
    fn cancel(self: Box<Self>) {
        let Self { task, slot } = *self;
        let _ignored = slot.cancel_unstarted();
        drop(task);
    }
}

/// Queued fire-and-forget runnable.
pub(crate) struct RunnableJob<T, E> {
    task: T,
    error: PhantomData<fn() -> E>,
}

impl<T, E> RunnableJob<T, E> {
    /// Creates a queued runnable job.
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
    /// Runnable jobs have no result endpoint to accept.
    fn accept(&self) {}

    /// Runs the detached runnable with panic conversion.
    fn run(self: Box<Self>) {
        let mut task = self.task;
        TaskRunner::new(move || task.run()).run_detached::<(), E>();
    }

    /// Drops the runnable without executing it.
    fn cancel(self: Box<Self>) {}
}
