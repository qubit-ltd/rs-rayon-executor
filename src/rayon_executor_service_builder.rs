// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::thread;

use rayon::ThreadPoolBuilder as RayonThreadPoolBuilder;

use crate::rayon_executor_service::RayonExecutorService;
use crate::rayon_executor_service_build_error::RayonExecutorServiceBuildError;
use crate::rayon_executor_service_state::RayonExecutorServiceState;

/// Default thread name prefix used by [`RayonExecutorServiceBuilder`].
const DEFAULT_THREAD_NAME_PREFIX: &str = "qubit-rayon-executor";

/// Builder for [`RayonExecutorService`].
///
/// The default builder uses the available CPU parallelism and names workers
/// with the `qubit-rayon-executor` prefix.
#[derive(Clone)]
pub struct RayonExecutorServiceBuilder {
    /// Number of Rayon worker threads to create.
    num_threads: usize,
    /// Prefix used when naming Rayon worker threads.
    thread_name_prefix: String,
    /// Optional worker stack size in bytes.
    stack_size: Option<usize>,
    /// Maximum number of accepted tasks that have not reached a terminal state.
    task_capacity: usize,
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

    /// Sets the maximum number of accepted unfinished tasks.
    pub fn task_capacity(mut self, task_capacity: usize) -> Self {
        self.task_capacity = task_capacity;
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
        if self.task_capacity == 0 {
            return Err(RayonExecutorServiceBuildError::ZeroTaskCapacity);
        }
        let prefix = self.thread_name_prefix;
        let mut builder = RayonThreadPoolBuilder::new()
            .num_threads(self.num_threads)
            .thread_name(move |index| format!("{prefix}-{index}"));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        let pool = std::sync::Arc::new(builder.build()?);
        let state = RayonExecutorServiceState::new(self.task_capacity, self.num_threads);
        Ok(RayonExecutorService { pool, state })
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
            task_capacity: 1024,
        }
    }
}

/// Returns the default Rayon thread count for new builders.
///
/// # Returns
///
/// The available CPU parallelism, or `1` if it cannot be detected.
fn default_rayon_thread_count() -> usize {
    thread::available_parallelism().map(usize::from).unwrap_or(1)
}
