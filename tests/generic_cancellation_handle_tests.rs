// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use qubit_executor::CancelResult;
use qubit_executor::CancellableTaskHandle;
use qubit_executor::TaskExecutionError;
use qubit_executor::TaskResult;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;

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
