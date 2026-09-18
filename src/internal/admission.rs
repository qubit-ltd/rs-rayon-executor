// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// A reserved task admission and the number of Rayon closures to dispatch.
pub(crate) struct Admission {
    /// Stable task identifier assigned to the accepted task.
    pub(crate) task_id: usize,
    /// Number of worker closures the caller must dispatch.
    pub(crate) dispatch_count: usize,
}
