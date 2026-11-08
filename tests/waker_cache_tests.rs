// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
#![cfg(feature = "async-wait")]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::RawWaker;
use std::task::RawWakerVTable;
use std::task::Wake;
use std::task::Waker;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

struct CloneCount(AtomicUsize);

unsafe fn clone_counted_waker(data: *const ()) -> RawWaker {
    // SAFETY: `data` is an Arc pointer created by `counted_waker`.
    let count = unsafe { &*data.cast::<CloneCount>() };
    count.0.fetch_add(1, Ordering::SeqCst);
    // SAFETY: Retain one strong reference for the cloned RawWaker.
    unsafe { Arc::increment_strong_count(data.cast::<CloneCount>()) };
    RawWaker::new(data, &COUNTED_WAKER_VTABLE)
}

unsafe fn wake_counted_waker(data: *const ()) {
    // SAFETY: This consumes the strong reference owned by the RawWaker.
    drop(unsafe { Arc::from_raw(data.cast::<CloneCount>()) });
}

unsafe fn wake_counted_waker_by_ref(_data: *const ()) {}

unsafe fn drop_counted_waker(data: *const ()) {
    // SAFETY: This consumes the strong reference owned by the RawWaker.
    drop(unsafe { Arc::from_raw(data.cast::<CloneCount>()) });
}

static COUNTED_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    clone_counted_waker,
    wake_counted_waker,
    wake_counted_waker_by_ref,
    drop_counted_waker,
);

fn counted_waker(count: &Arc<CloneCount>) -> Waker {
    let data = Arc::into_raw(Arc::clone(count)).cast::<()>();
    // SAFETY: The vtable preserves the Arc reference represented by `data`.
    unsafe { Waker::from_raw(RawWaker::new(data, &COUNTED_WAKER_VTABLE)) }
}

struct CountWake(AtomicUsize);

impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn poll_changed(
    changed: Pin<&mut impl Future<Output = Result<(), tokio::sync::watch::error::RecvError>>>,
    waker: &Waker,
) -> Poll<Result<(), tokio::sync::watch::error::RecvError>> {
    Future::poll(changed, &mut Context::from_waker(waker))
}

#[test]
fn test_same_waker_pending_polls_reuse_proxy_allocation() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let mut changes = service.capacity_changes();
    let mut changed = Box::pin(changes.changed());
    let clone_count = Arc::new(CloneCount(AtomicUsize::new(0)));
    let waker = counted_waker(&clone_count);
    assert!(poll_changed(changed.as_mut(), &waker).is_pending());

    let clones_after_first_poll = clone_count.0.load(Ordering::SeqCst);
    for _ in 0..100 {
        assert!(poll_changed(changed.as_mut(), &waker).is_pending());
    }
    assert_eq!(
        clone_count.0.load(Ordering::SeqCst),
        clones_after_first_poll,
        "same-waker pending polls should reuse the proxy"
    );
    drop(changed);
    drop(changes);
    service.shutdown();
}

#[test]
fn test_changed_waker_receives_notification_and_old_waker_does_not() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let mut changes = service.capacity_changes();
    let mut changed = Box::pin(changes.changed());
    let wake_a = Arc::new(CountWake(AtomicUsize::new(0)));
    let wake_b = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker_a = Waker::from(Arc::clone(&wake_a));
    let waker_b = Waker::from(Arc::clone(&wake_b));

    assert!(poll_changed(changed.as_mut(), &waker_a).is_pending());
    assert!(poll_changed(changed.as_mut(), &waker_b).is_pending());
    service.shutdown();

    assert_eq!(wake_a.0.load(Ordering::SeqCst), 0, "replaced waker must not be called");
    assert!(
        wake_b.0.load(Ordering::SeqCst) > 0,
        "latest waker should receive notification"
    );
    assert!(poll_changed(changed.as_mut(), &waker_b).is_ready());
    drop(changed);
    drop(changes);
}

#[test]
fn test_dropping_pending_waiter_releases_cached_waker() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let mut changes = service.capacity_changes();
    let mut changed = Box::pin(changes.changed());
    let wake = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&wake));
    let baseline_count = Arc::strong_count(&wake);
    assert!(poll_changed(changed.as_mut(), &waker).is_pending());
    assert!(
        Arc::strong_count(&wake) > baseline_count,
        "pending waiter should retain its caller waker"
    );
    drop(changed);
    assert_eq!(
        Arc::strong_count(&wake),
        baseline_count,
        "dropping waiter should release cached waker"
    );
    drop(changes);
    service.shutdown();
}

#[test]
fn test_notification_wakes_cached_waker() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("service should build");
    let mut changes = service.capacity_changes();
    let mut changed = Box::pin(changes.changed());
    let wake = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&wake));
    assert!(poll_changed(changed.as_mut(), &waker).is_pending());
    service.shutdown();

    assert!(
        wake.0.load(Ordering::SeqCst) > 0,
        "notification should wake the registered caller"
    );
    assert!(poll_changed(changed.as_mut(), &waker).is_ready());
    drop(changed);
    drop(changes);
}
