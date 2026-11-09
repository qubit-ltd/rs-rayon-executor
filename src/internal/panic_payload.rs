// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::any::Any;

/// Drops a panic payload without allowing its destructor to unwind again.
///
/// The original payload is dropped normally. If that destructor panics, the
/// secondary panic payload is intentionally forgotten so its destructor cannot
/// trigger another unwind. This may leak resources owned by that secondary
/// payload; it cannot recover from aborting panics or a destructor that never
/// returns.
///
/// # Parameters
///
/// - `payload`: The panic payload to drop while containing a secondary panic.
pub(crate) fn discard_panic_payload(payload: Box<dyn Any + Send>) {
    if let Err(secondary) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
        std::mem::forget(secondary);
    }
}
