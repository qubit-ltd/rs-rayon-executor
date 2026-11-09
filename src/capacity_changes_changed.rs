// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use tokio::sync::watch;

use crate::internal::IsolatedWakers;
use crate::internal::poll_with_waker_panic_isolation;

/// Future returned by [`crate::CapacityChanges::changed`].
///
/// # Examples
///
/// ```
/// use qubit_executor::service::ExecutorService;
/// use qubit_rayon_executor::CapacityChangesChanged;
/// use qubit_rayon_executor::RayonExecutorService;
///
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let service = RayonExecutorService::builder().num_threads(1).build()?;
///     let mut changes = service.capacity_changes();
///     let runtime = tokio::runtime::Builder::new_current_thread()
///         .enable_all()
///         .build()?;
///     let changed: CapacityChangesChanged<'_> = changes.changed();
///     service.shutdown();
///     runtime.block_on(changed)?;
///     Ok(())
/// }
/// ```
pub struct CapacityChangesChanged<'a> {
    /// Watch future polled with a panic-isolating waker.
    future: Pin<Box<dyn Future<Output = Result<(), watch::error::RecvError>> + Send + 'a>>,
    /// Cached caller and proxy wakers for this future.
    wakers: IsolatedWakers,
}

/// Implements polling for the capacity notification future.
impl<'a> CapacityChangesChanged<'a> {
    /// Creates an isolated capacity-change future.
    ///
    /// # Parameters
    ///
    /// * `future` - The Tokio watch future to poll.
    ///
    /// # Returns
    ///
    /// A notification future that isolates panics from its caller's waker.
    pub(crate) fn new(future: Pin<Box<dyn Future<Output = Result<(), watch::error::RecvError>> + Send + 'a>>) -> Self {
        Self {
            future,
            wakers: IsolatedWakers::default(),
        }
    }
}

/// Polls the wrapped notification until a capacity change is observed.
impl Future for CapacityChangesChanged<'_> {
    /// The result returned by Tokio when the channel changes or closes.
    type Output = Result<(), watch::error::RecvError>;

    /// Polls while containing panics from the caller's waker.
    ///
    /// # Parameters
    ///
    /// * `context` - The task context whose waker is safely proxied.
    ///
    /// # Returns
    ///
    /// `Poll::Ready` when the channel changes or closes, or `Poll::Pending`
    /// while waiting for another notification.
    #[inline]
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        poll_with_waker_panic_isolation(&mut this.wakers, this.future.as_mut(), context)
    }
}
