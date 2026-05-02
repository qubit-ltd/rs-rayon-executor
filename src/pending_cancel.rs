/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::Arc;

/// Shared cancellation callback stored for pending Rayon tasks.
pub(crate) type PendingCancel = Arc<dyn Fn() -> bool + Send + Sync + 'static>;
