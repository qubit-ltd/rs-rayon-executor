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
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        poll_with_waker_panic_isolation(&mut this.wakers, this.future.as_mut(), context)
    }
}
