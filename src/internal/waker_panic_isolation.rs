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

use crate::internal::IsolatedWakers;

/// Future adapter that contains panics from a task's waker.
///
/// # Type Parameters
///
/// * `F` - The future being polled with an isolated waker.
pub(crate) struct WakerPanicIsolation<F> {
    /// Future polled with the proxy waker.
    future: Pin<Box<F>>,
    /// Caller and proxy wakers retained for this future's repeated polls.
    wakers: IsolatedWakers,
}

/// Constructs a future adapter with the requested inner future.
impl<F> WakerPanicIsolation<F> {
    /// Wraps a future so notification callbacks cannot unwind through its
    /// producer.
    ///
    /// # Parameters
    ///
    /// * `future` - The future that may register a waker.
    ///
    /// # Returns
    ///
    /// An adapter that polls `future` with a panic-isolating waker proxy.
    pub(crate) fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
            wakers: IsolatedWakers::default(),
        }
    }
}

/// Polls the inner future with a panic-isolating waker.
impl<F: Future> Future for WakerPanicIsolation<F> {
    /// The output produced by the wrapped future.
    type Output = F::Output;

    /// Polls the wrapped future with a proxy for the caller's waker.
    ///
    /// # Parameters
    ///
    /// * `context` - The caller's task context.
    ///
    /// # Returns
    ///
    /// The wrapped future's `Poll` result, including `Pending` while it waits.
    #[inline]
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        poll_with_waker_panic_isolation(&mut this.wakers, this.future.as_mut(), context)
    }
}

/// Polls a future with a proxy that contains panics from its registered waker.
///
/// # Type Parameters
///
/// * `F` - The future being polled.
///
/// # Parameters
///
/// * `future` - Pinned future to poll.
/// * `context` - The caller's task context.
/// * `wakers` - The per-future cache of caller and proxy wakers.
///
/// # Returns
///
/// The result from polling the wrapped future.
pub(crate) fn poll_with_waker_panic_isolation<F: Future + ?Sized>(
    wakers: &mut IsolatedWakers,
    future: Pin<&mut F>,
    context: &mut Context<'_>,
) -> Poll<F::Output> {
    let proxy = wakers.proxy_for(context.waker());
    let mut isolated_context = Context::from_waker(proxy);
    future.poll(&mut isolated_context)
}
