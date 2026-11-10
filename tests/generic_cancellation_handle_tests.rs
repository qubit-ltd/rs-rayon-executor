// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::future::Future;
use std::future::IntoFuture;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::task::Context;
use std::task::Wake;
use std::task::Waker;
use std::time::Duration;

use qubit_executor::CancelResult;
use qubit_executor::CancellableTaskHandle;
use qubit_executor::TaskExecutionError;
use qubit_executor::TaskResult;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

struct ReleaseGuard(Option<mpsc::SyncSender<()>>);

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

struct CancelOnWake {
    endpoint: Mutex<Option<qubit_executor::TaskCancellationHandle>>,
    service: Arc<RayonExecutorService>,
    report: mpsc::SyncSender<(CancelResult, usize)>,
}

impl Wake for CancelOnWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(endpoint) = self.endpoint.lock().expect("endpoint lock").take() {
            let result = endpoint.cancel();
            let running = self.service.stats().running;
            let _ = self.report.send((result, running));
        }
    }
}

#[test]
fn test_terminal_result_waker_cancels_endpoint_as_finished() {
    let service = Arc::new(
        RayonExecutorService::builder()
            .num_threads(1)
            .build()
            .expect("service should build"),
    );
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let mut release_guard = ReleaseGuard(Some(release_tx));
    let handle = service
        .submit_tracked_callable(move || {
            started_tx.send(()).expect("test should still be waiting");
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("test should release callable");
            Ok::<usize, io::Error>(42)
        })
        .expect("task should be accepted");
    let endpoint = handle.cancellation_handle();
    let mut future = Box::pin(handle.into_future());
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("callable should start");
    let (report_tx, report_rx) = mpsc::sync_channel(1);
    let waker = Waker::from(Arc::new(CancelOnWake {
        endpoint: Mutex::new(Some(endpoint)),
        service: Arc::clone(&service),
        report: report_tx,
    }));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        Pin::as_mut(&mut future).poll(&mut context),
        std::task::Poll::Pending
    ));

    release_guard
        .0
        .take()
        .expect("release sender retained")
        .send(())
        .expect("callable should be released");
    let (cancel_result, running) = report_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("result wake should report cancellation");
    assert_eq!((cancel_result, running), (CancelResult::AlreadyFinished, 1));
    assert!(matches!(
        Pin::as_mut(&mut future).poll(&mut context),
        std::task::Poll::Ready(Ok(42))
    ));

    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    assert_eq!(service.stats().running, 0);
    assert_eq!(service.stats().queued, 0);
    assert_eq!(service.stats().cancelling, 0);
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

async fn cancel_after_timeout<H>(task: H) -> TaskResult<usize, io::Error>
where
    H: CancellableTaskHandle<usize, io::Error> + std::future::IntoFuture<Output = TaskResult<usize, io::Error>>,
{
    let cancellation = CancellableTaskHandle::cancellation_handle(&task);
    let mut result = Box::pin(task.into_future());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), result.as_mut())
            .await
            .is_err()
    );
    assert_eq!(cancellation.cancel(), CancelResult::Cancelled);
    tokio::time::timeout(Duration::from_secs(5), result)
        .await
        .expect("cancelled result should arrive within timeout")
}

#[test]
fn test_generic_cancellation_handle_preserves_rayon_job_cleanup_and_capacity() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(2)
        .build()
        .expect("service should build");
    let (running, release_tx) = common::helpers::submit_blocking_task(&service);

    let drops = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let queued = service
        .submit_tracked_callable({
            let probe = DropProbe(Arc::clone(&drops));
            let executions = Arc::clone(&executions);
            move || {
                let _probe = &probe;
                executions.fetch_add(1, Ordering::SeqCst);
                Ok::<usize, io::Error>(42)
            }
        })
        .expect("queued task should be accepted");
    assert!(matches!(
        service.submit_callable(|| Ok::<usize, io::Error>(0)),
        Err(SubmissionError::Saturated)
    ));

    let cancelled = common::helpers::create_runtime().block_on(cancel_after_timeout(queued));
    assert!(matches!(cancelled, Err(TaskExecutionError::Cancelled)));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(service.stats().queued, 0);
    assert_eq!(service.stats().cancelling, 0);

    let follow_up = service
        .submit_tracked_callable(|| Ok::<usize, io::Error>(7))
        .expect("capacity should be available after queued-job cleanup");
    release_tx.send(()).expect("running task should release");
    assert_eq!(follow_up.get().expect("follow-up task should run"), 7);
    service.shutdown();
    assert!(service.wait_termination_timeout(Duration::from_secs(10)));
    running.get().expect("running task should finish");
}
