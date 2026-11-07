// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::future::Future;
use std::pin::Pin;

use tokio::sync::watch;

use crate::CapacityChangesChanged;

/// Receiver for capacity changes that may make another task admissible.
///
/// Notifications are advisory; callers must retry submission because another
/// producer may consume an available slot first. Panics from a registered
/// waker are contained so they cannot interrupt notification of other waiters.
///
/// This type is available with the `async-wait` feature.
///
/// # Examples
///
/// ```
/// use qubit_executor::service::ExecutorService;
/// use qubit_rayon_executor::RayonExecutorService;
///
/// let service = RayonExecutorService::builder().num_threads(1).build()?;
/// let mut changes = service.capacity_changes();
/// let initial = changes.generation();
/// assert!(!changes.has_changed()?);
/// service.shutdown();
/// assert!(changes.has_changed()?);
/// let current = changes.generation_and_update();
/// assert_ne!(initial, current);
/// assert!(!changes.has_changed()?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// See the [user guide](https://github.com/qubit-ltd/rs-rayon-executor/blob/main/doc/user_guide.md)
/// for the capacity notification workflow.
#[derive(Clone)]
pub struct CapacityChanges {
    /// Tokio watch receiver wrapped to isolate task waker panics.
    receiver: watch::Receiver<u64>,
}

/// Provides the notification receiver's public operations.
impl CapacityChanges {
    /// Waits for a new capacity or lifecycle notification.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the watched generation changes, or Tokio's
    /// [`watch::error::RecvError`] if the service state has been dropped.
    pub fn changed(&mut self) -> CapacityChangesChanged<'_> {
        let future: Pin<Box<dyn Future<Output = Result<(), watch::error::RecvError>> + Send + '_>> =
            Box::pin(self.receiver.changed());
        CapacityChangesChanged::new(future)
    }

    /// Returns whether the receiver has an unseen notification.
    pub fn has_changed(&self) -> Result<bool, watch::error::RecvError> {
        self.receiver.has_changed()
    }

    /// Reads the current generation without marking it as seen.
    ///
    /// The returned watch guard holds a read lock. Keep it short lived: copy
    /// the value and drop the guard before awaiting, waiting for termination,
    /// or calling an operation such as shutdown, stop, or cancel that may
    /// publish a notification.
    /// Holding the guard across such an operation can block that operation
    /// indefinitely. The generation is a wrapping notification token, not a
    /// count of available capacity.
    pub fn borrow(&self) -> watch::Ref<'_, u64> {
        self.receiver.borrow()
    }

    /// Reads the current generation and marks it as seen.
    ///
    /// The returned watch guard holds a read lock. Keep it short lived: copy
    /// the value and drop the guard before awaiting, waiting for termination,
    /// or calling an operation such as shutdown, stop, or cancel that may
    /// publish a notification.
    /// Holding the guard across such an operation can block that operation
    /// indefinitely. The generation is a wrapping notification token, not a
    /// count of available capacity.
    pub fn borrow_and_update(&mut self) -> watch::Ref<'_, u64> {
        self.receiver.borrow_and_update()
    }

    /// Returns a copied notification generation without marking it as seen.
    ///
    /// The generation is a wrapping notification token, not a count of
    /// available capacity. A notification does not guarantee that a capacity
    /// slot will still be available when a task is submitted.
    pub fn generation(&self) -> u64 {
        *self.receiver.borrow()
    }

    /// Returns a copied notification generation and marks it as seen.
    ///
    /// The generation is a wrapping notification token, not a count of
    /// available capacity. A notification does not guarantee that a capacity
    /// slot will still be available when a task is submitted.
    pub fn generation_and_update(&mut self) -> u64 {
        *self.receiver.borrow_and_update()
    }

    /// Returns whether the underlying channel is closed.
    pub fn is_closed(&self) -> bool {
        self.receiver.is_closed()
    }

    /// Marks the current generation as unseen.
    pub fn mark_changed(&mut self) {
        self.receiver.mark_changed();
    }

    /// Marks the current generation as seen.
    pub fn mark_unchanged(&mut self) {
        self.receiver.mark_unchanged();
    }

    /// Returns whether two receivers observe the same channel.
    ///
    /// # Parameters
    ///
    /// * `other` - The receiver to compare with this receiver.
    ///
    /// # Returns
    ///
    /// `true` if both receivers observe the same underlying channel.
    pub fn same_channel(&self, other: &Self) -> bool {
        self.receiver.same_channel(&other.receiver)
    }
}

/// Converts a Tokio watch receiver into the public capacity notification type.
impl From<watch::Receiver<u64>> for CapacityChanges {
    /// Creates the wrapper from a subscribed Tokio receiver.
    ///
    /// # Parameters
    ///
    /// * `receiver` - The subscribed watch receiver.
    fn from(receiver: watch::Receiver<u64>) -> Self {
        Self { receiver }
    }
}
