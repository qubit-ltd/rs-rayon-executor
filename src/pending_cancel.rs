/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::sync::Arc;

/// Shared cancellation callback stored for pending Rayon tasks.
pub(crate) type PendingCancel = Arc<dyn Fn() -> bool + Send + Sync + 'static>;
