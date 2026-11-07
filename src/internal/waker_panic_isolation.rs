// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

/// Future adapter that contains panics from a task's waker.
///
/// # Type Parameters
///
/// * `F` - The future being polled with an isolated waker.
pub(crate) struct WakerPanicIsolation<F> {
    /// Future polled with the proxy waker.
    future: Pin<Box<F>>,
}

/// Constructs a future adapter with the requested inner future.
impl<F> WakerPanicIsolation<F> {
    /// Wraps a future so notification callbacks cannot unwind through its
    /// producer.
    ///
    /// # Parameters
    ///
    /// * `future` - The future that may register a waker.
    pub(crate) fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
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
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        poll_with_waker_panic_isolation(this.future.as_mut(), context)
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
///
/// # Returns
///
/// The result from polling the wrapped future.
pub(crate) fn poll_with_waker_panic_isolation<F: Future + ?Sized>(
    future: Pin<&mut F>,
    context: &mut Context<'_>,
) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(crate::internal::PanicIsolatingWake::new(
        context.waker().clone(),
    )));
    let mut isolated_context = Context::from_waker(&waker);
    future.poll(&mut isolated_context)
}
