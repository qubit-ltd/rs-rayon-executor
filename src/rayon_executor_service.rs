/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        Mutex,
        MutexGuard,
        atomic::{
            AtomicUsize,
            Ordering,
        },
    },
    task::{
        Context,
        Poll,
    },
    thread,
};

use qubit_atomic::{
    Atomic,
    AtomicCount,
};
use qubit_function::Callable;
use rayon::{
    ThreadPool as RayonThreadPool,
    ThreadPoolBuilder as RayonThreadPoolBuilder,
};
use thiserror::Error;
use tokio::sync::Notify;

use qubit_executor::{
    TaskCompletionPair,
    TaskHandle,
    TaskRunner,
    TaskResult,
};

use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
    ShutdownReport,
};

/// Default thread name prefix used by [`RayonExecutorServiceBuilder`].
const DEFAULT_THREAD_NAME_PREFIX: &str = "qubit-rayon-executor";

/// Shared cancellation callback stored for pending Rayon tasks.
type PendingCancel = Arc<dyn Fn() -> bool + Send + Sync + 'static>;

/// Error returned when [`RayonExecutorServiceBuilder`] cannot build a service.
#[derive(Debug, Error)]
pub enum RayonExecutorServiceBuildError {
    /// The configured Rayon thread count is zero.
    #[error("rayon executor service thread count must be greater than zero")]
    ZeroThreadCount,

    /// The configured worker stack size is zero.
    #[error("rayon executor service stack size must be greater than zero")]
    ZeroStackSize,

    /// Rayon rejected the underlying thread-pool configuration.
    #[error("failed to build rayon executor service: {source}")]
    BuildFailed {
        /// Rayon build error returned by the underlying thread-pool builder.
        #[from]
        source: rayon::ThreadPoolBuildError,
    },
}

/// Builder for [`RayonExecutorService`].
///
/// The default builder uses the available CPU parallelism and names workers
/// with the `qubit-rayon-executor` prefix.
#[derive(Debug, Clone)]
pub struct RayonExecutorServiceBuilder {
    /// Number of Rayon worker threads to create.
    num_threads: usize,
    /// Prefix used when naming Rayon worker threads.
    thread_name_prefix: String,
    /// Optional worker stack size in bytes.
    stack_size: Option<usize>,
}

impl RayonExecutorServiceBuilder {
    /// Sets the number of Rayon worker threads.
    ///
    /// # Parameters
    ///
    /// * `num_threads` - Number of Rayon worker threads.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn num_threads(mut self, num_threads: usize) -> Self {
        self.num_threads = num_threads;
        self
    }

    /// Sets the Rayon worker-thread name prefix.
    ///
    /// # Parameters
    ///
    /// * `prefix` - Prefix appended with the worker index.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn thread_name_prefix(mut self, prefix: &str) -> Self {
        self.thread_name_prefix = prefix.to_owned();
        self
    }

    /// Sets the Rayon worker-thread stack size.
    ///
    /// # Parameters
    ///
    /// * `stack_size` - Stack size in bytes for each Rayon worker.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Builds the configured Rayon executor service.
    ///
    /// # Returns
    ///
    /// `Ok(RayonExecutorService)` if the Rayon thread pool is created
    /// successfully.
    ///
    /// # Errors
    ///
    /// Returns [`RayonExecutorServiceBuildError`] if the thread count or stack
    /// size is zero, or if Rayon rejects the thread-pool configuration.
    pub fn build(self) -> Result<RayonExecutorService, RayonExecutorServiceBuildError> {
        if self.num_threads == 0 {
            return Err(RayonExecutorServiceBuildError::ZeroThreadCount);
        }
        if self.stack_size == Some(0) {
            return Err(RayonExecutorServiceBuildError::ZeroStackSize);
        }
        let prefix = self.thread_name_prefix;
        let mut builder = RayonThreadPoolBuilder::new()
            .num_threads(self.num_threads)
            .thread_name(move |index| format!("{prefix}-{index}"));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        let pool = Arc::new(builder.build()?);
        Ok(RayonExecutorService {
            pool,
            state: Arc::new(RayonExecutorServiceState::new()),
        })
    }
}

impl Default for RayonExecutorServiceBuilder {
    /// Creates a builder with CPU-parallelism defaults.
    ///
    /// # Returns
    ///
    /// A builder configured for the detected CPU parallelism.
    fn default() -> Self {
        Self {
            num_threads: default_rayon_thread_count(),
            thread_name_prefix: DEFAULT_THREAD_NAME_PREFIX.to_owned(),
            stack_size: None,
        }
    }
}

/// Shared state for [`RayonExecutorService`].
struct RayonExecutorServiceState {
    /// Whether shutdown has been requested.
    shutdown: Atomic<bool>,
    /// Number of accepted tasks that have not yet completed or been cancelled.
    active_tasks: AtomicCount,
    /// Number of accepted tasks that have not started running yet.
    queued_tasks: AtomicCount,
    /// Serializes task submission and shutdown transitions.
    submission_lock: Mutex<()>,
    /// Cancellation hooks for tasks that have not started yet.
    pending_tasks: Mutex<HashMap<usize, PendingCancel>>,
    /// Monotonic identifier assigned to each accepted task.
    next_task_id: AtomicUsize,
    /// Notifies waiters once shutdown has completed and no tasks remain active.
    terminated_notify: Notify,
}

impl RayonExecutorServiceState {
    /// Creates fresh shared state for a Rayon-backed executor service.
    ///
    /// # Returns
    ///
    /// Shared service state with zero accepted tasks and running lifecycle.
    fn new() -> Self {
        Self {
            shutdown: Atomic::new(false),
            active_tasks: AtomicCount::new(0),
            queued_tasks: AtomicCount::new(0),
            submission_lock: Mutex::new(()),
            pending_tasks: Mutex::new(HashMap::new()),
            next_task_id: AtomicUsize::new(0),
            terminated_notify: Notify::new(),
        }
    }

    /// Acquires the submission lock while tolerating poisoned locks.
    ///
    /// # Returns
    ///
    /// A guard for the submission lock.
    fn lock_submission(&self) -> MutexGuard<'_, ()> {
        self.submission_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquires the pending-task map while tolerating poisoned locks.
    ///
    /// # Returns
    ///
    /// A guard for the pending-task cancellation map.
    fn lock_pending_tasks(&self) -> MutexGuard<'_, HashMap<usize, PendingCancel>> {
        self.pending_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Allocates the next stable task identifier.
    ///
    /// # Returns
    ///
    /// A unique task identifier within this service instance.
    fn next_task_id(&self) -> usize {
        self.next_task_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Registers a pending task cancellation hook.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the accepted task.
    /// * `cancel` - Callback used to cancel the task before it starts.
    fn register_pending_task(&self, task_id: usize, cancel: PendingCancel) {
        self.lock_pending_tasks().insert(task_id, cancel);
    }

    /// Removes a pending task registration if it still exists.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the task to remove.
    fn remove_pending_task(&self, task_id: usize) {
        self.lock_pending_tasks().remove(&task_id);
    }

    /// Drains all pending-task cancellation hooks.
    ///
    /// # Returns
    ///
    /// A vector containing every pending-task cancellation hook that was
    /// registered at the time of the drain.
    fn drain_pending_tasks(&self) -> Vec<PendingCancel> {
        self.lock_pending_tasks()
            .drain()
            .map(|(_, cancel)| cancel)
            .collect()
    }

    /// Updates counters after an accepted task starts running.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the task that just started.
    fn on_task_started(&self, task_id: usize) {
        self.remove_pending_task(task_id);
        self.queued_tasks.dec();
    }

    /// Updates counters after a pending task is cancelled before start.
    ///
    /// # Parameters
    ///
    /// * `task_id` - Stable identifier of the cancelled task.
    fn on_task_cancelled(&self, task_id: usize) {
        self.remove_pending_task(task_id);
        self.queued_tasks.dec();
        if self.active_tasks.dec() == 0 {
            self.notify_if_terminated();
        }
    }

    /// Updates counters after a started task completes.
    fn on_task_completed(&self) {
        if self.active_tasks.dec() == 0 {
            self.notify_if_terminated();
        }
    }

    /// Wakes termination waiters when shutdown and task completion allow it.
    fn notify_if_terminated(&self) {
        if self.shutdown.load() && self.active_tasks.is_zero() {
            self.terminated_notify.notify_waiters();
        }
    }
}

/// Handle returned by [`RayonExecutorService`] for accepted tasks.
///
/// This handle supports blocking [`Self::get`], asynchronous `.await`, and
/// best-effort cancellation before a Rayon worker starts the task.
pub struct RayonTaskHandle<R, E> {
    /// Shared task result observed through blocking and async APIs.
    inner: TaskHandle<R, E>,
    /// Cancellation hook that wins only before task start.
    cancel: PendingCancel,
}

impl<R, E> RayonTaskHandle<R, E> {
    /// Creates a Rayon task handle from a shared task handle and cancel hook.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared task handle used for result observation.
    /// * `cancel` - Cancellation hook that may cancel the task before start.
    ///
    /// # Returns
    ///
    /// A handle for the accepted Rayon task.
    fn new(inner: TaskHandle<R, E>, cancel: PendingCancel) -> Self {
        Self { inner, cancel }
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
        (self.cancel)()
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

/// Rayon-backed executor service for CPU-bound synchronous tasks.
///
/// Accepted tasks are executed on a dedicated Rayon thread pool. The service
/// preserves the crate's `ExecutorService` lifecycle semantics and task-handle
/// APIs while delegating scheduling to Rayon.
#[derive(Clone)]
pub struct RayonExecutorService {
    /// Rayon thread pool used to execute accepted tasks.
    pool: Arc<RayonThreadPool>,
    /// Shared lifecycle and cancellation state.
    state: Arc<RayonExecutorServiceState>,
}

impl RayonExecutorService {
    /// Creates a Rayon executor service with default builder settings.
    ///
    /// # Returns
    ///
    /// `Ok(RayonExecutorService)` if the default Rayon thread pool can be
    /// built.
    ///
    /// # Errors
    ///
    /// Returns [`RayonExecutorServiceBuildError`] if the default builder
    /// configuration is rejected.
    #[inline]
    pub fn new() -> Result<Self, RayonExecutorServiceBuildError> {
        Self::builder().build()
    }

    /// Creates a builder for configuring a Rayon executor service.
    ///
    /// # Returns
    ///
    /// A builder configured with CPU-parallelism defaults.
    #[inline]
    pub fn builder() -> RayonExecutorServiceBuilder {
        RayonExecutorServiceBuilder::default()
    }
}

impl ExecutorService for RayonExecutorService {
    type Handle<R, E>
        = RayonTaskHandle<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    type Termination<'a>
        = Pin<Box<dyn Future<Output = ()> + Send + 'a>>
    where
        Self: 'a;

    /// Accepts a callable and schedules it on the Rayon thread pool.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable to execute on a Rayon worker.
    ///
    /// # Returns
    ///
    /// A [`RayonTaskHandle`] for the accepted task.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::Shutdown`] if shutdown has already been
    /// requested before the task is accepted.
    fn submit_callable<C, R, E>(&self, task: C) -> Result<Self::Handle<R, E>, RejectedExecution>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let submission_guard = self.state.lock_submission();
        if self.state.shutdown.load() {
            return Err(RejectedExecution::Shutdown);
        }
        let task_id = self.state.next_task_id();
        self.state.active_tasks.inc();
        self.state.queued_tasks.inc();
        let (handle, completion) = TaskCompletionPair::new().into_parts();
        let completion_for_cancel = completion.clone();
        let state_for_cancel = Arc::clone(&self.state);
        let cancel: PendingCancel = Arc::new(move || {
            if completion_for_cancel.cancel() {
                state_for_cancel.on_task_cancelled(task_id);
                true
            } else {
                false
            }
        });
        self.state
            .register_pending_task(task_id, Arc::clone(&cancel));
        drop(submission_guard);

        let completion_for_run = completion;
        let state_for_run = Arc::clone(&self.state);
        self.pool.spawn_fifo(move || {
            if !completion_for_run.start() {
                return;
            }
            state_for_run.on_task_started(task_id);
            completion_for_run.complete(TaskRunner::new(task).call());
            state_for_run.on_task_completed();
        });
        Ok(RayonTaskHandle::new(handle, cancel))
    }

    /// Stops accepting new tasks.
    ///
    /// Already accepted Rayon tasks are allowed to finish normally.
    fn shutdown(&self) {
        let _guard = self.state.lock_submission();
        self.state.shutdown.store(true);
        self.state.notify_if_terminated();
    }

    /// Stops accepting new tasks and cancels tasks that have not started yet.
    ///
    /// Running Rayon tasks cannot be preempted. Cancellation therefore applies
    /// only to tasks that are still pending when the cancellation hook wins the
    /// race against task start.
    ///
    /// # Returns
    ///
    /// A count-based report describing the pending and running work observed at
    /// the time of the shutdown request, plus the number of pending tasks for
    /// which cancellation succeeded.
    fn shutdown_now(&self) -> ShutdownReport {
        let _guard = self.state.lock_submission();
        self.state.shutdown.store(true);
        let queued = self.state.queued_tasks.get();
        let running = self.state.active_tasks.get().saturating_sub(queued);
        let pending = self.state.drain_pending_tasks();
        drop(_guard);

        let mut cancelled = 0usize;
        for cancel in pending {
            if cancel() {
                cancelled += 1;
            }
        }
        self.state.notify_if_terminated();
        ShutdownReport::new(queued, running, cancelled)
    }

    /// Returns whether shutdown has been requested.
    fn is_shutdown(&self) -> bool {
        self.state.shutdown.load()
    }

    /// Returns whether shutdown was requested and no accepted tasks remain.
    fn is_terminated(&self) -> bool {
        self.is_shutdown() && self.state.active_tasks.is_zero()
    }

    /// Waits until the service has terminated.
    ///
    /// # Returns
    ///
    /// A future that resolves after shutdown has been requested and all
    /// accepted Rayon tasks have completed or been cancelled before start.
    fn await_termination(&self) -> Self::Termination<'_> {
        Box::pin(async move {
            loop {
                if self.is_terminated() {
                    return;
                }
                self.state.terminated_notify.notified().await;
            }
        })
    }
}

/// Returns the default Rayon thread count for new builders.
///
/// # Returns
///
/// The available CPU parallelism, or `1` if it cannot be detected.
fn default_rayon_thread_count() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}
