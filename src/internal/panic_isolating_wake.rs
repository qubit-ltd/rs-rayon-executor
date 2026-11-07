// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::task::Wake;
use std::task::Waker;

use crate::internal::discard_panic_payload;

/// Waker proxy that contains panics from the wrapped task waker.
pub(crate) struct PanicIsolatingWake {
    /// Waker supplied by the task executor.
    waker: Waker,
}

/// Creates the panic-isolating proxy.
impl PanicIsolatingWake {
    /// Creates a proxy for a task waker.
    ///
    /// # Parameters
    ///
    /// * `waker` - The task waker whose panic must be contained.
    pub(crate) fn new(waker: Waker) -> Self {
        Self { waker }
    }
}

/// Forwards wake notifications while containing callback panics.
impl Wake for PanicIsolatingWake {
    /// Consumes the proxy while forwarding a wake notification.
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    /// Forwards a wake notification and safely discards any panic payload.
    ///
    /// # Parameters
    ///
    /// * `self` - Shared proxy holding the task waker.
    fn wake_by_ref(self: &Arc<Self>) {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.waker.wake_by_ref();
        })) {
            discard_panic_payload(payload);
        }
    }
}
