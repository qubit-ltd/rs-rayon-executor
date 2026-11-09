// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::task::Waker;

/// Cached panic-isolating proxy associated with one future.
#[derive(Default)]
pub(crate) struct IsolatedWakers {
    /// The caller waker used to create the cached proxy.
    caller: Option<Waker>,
    /// Proxy registered with the wrapped future.
    proxy: Option<Waker>,
}

impl IsolatedWakers {
    /// Returns the cached proxy when the caller waker is equivalent, creating
    /// a new proxy when the caller changes.
    ///
    /// # Parameters
    ///
    /// * `caller` - The current task waker.
    ///
    /// # Returns
    ///
    /// The cached proxy for `caller`, borrowed from this cache.
    pub(crate) fn proxy_for(&mut self, caller: &Waker) -> &Waker {
        if !self.caller.as_ref().is_some_and(|cached| cached.will_wake(caller)) {
            self.caller = Some(caller.clone());
            self.proxy = Some(Waker::from(Arc::new(crate::internal::PanicIsolatingWake::new(
                caller.clone(),
            ))));
        }
        self.proxy.as_ref().expect("proxy is initialized with its caller waker")
    }
}
