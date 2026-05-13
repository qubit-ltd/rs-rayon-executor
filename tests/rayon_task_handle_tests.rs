/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`RayonTaskHandle`](qubit_rayon_executor::service::RayonTaskHandle).

mod common;

use std::io;

use qubit_executor::service::ExecutorService;
use qubit_executor::task::spi::{
    TaskResultHandle,
    TrackedTaskHandle,
};
use qubit_executor::{
    CancelResult,
    TaskExecutionError,
    TaskStatus,
    TryGet,
};
use qubit_rayon_executor::RayonExecutorService;

use crate::common::helpers::{
    create_single_worker_service,
    ok_usize_task,
    submit_blocking_task,
    wait_until,
};

#[tokio::test]
async fn test_rayon_task_handle_can_be_awaited() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    assert_eq!(handle.await.expect("handle should await result"), 42);
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_rayon_task_handle_cancel_before_start_reports_cancelled() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);
    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(queued.is_done());
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    service.shutdown();
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    service.wait_termination();
}

#[test]
fn test_rayon_task_handle_reports_panicked_task() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_tracked(|| -> Result<(), io::Error> { panic!("rayon service panic") })
        .expect("service should accept panicking task");

    assert!(matches!(handle.get(), Err(TaskExecutionError::Panicked)));
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_rayon_task_handle_cancel_after_completion_returns_false() {
    let service = create_single_worker_service();
    let handle = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    wait_until(|| handle.is_done());
    assert_eq!(handle.cancel(), CancelResult::AlreadyFinished);
    assert_eq!(handle.get().expect("callable should complete"), 42);
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_rayon_task_handle_cancel_running_reports_already_running() {
    let service = create_single_worker_service();
    let (handle, release_tx) = submit_blocking_task(&service);

    assert_eq!(
        <_ as TrackedTaskHandle<(), io::Error>>::status(&handle),
        TaskStatus::Running,
    );
    assert_eq!(
        <_ as TrackedTaskHandle<(), io::Error>>::cancel(&handle),
        CancelResult::AlreadyRunning,
    );
    assert!(!<_ as TaskResultHandle<(), io::Error>>::is_done(&handle));

    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    <_ as TaskResultHandle<(), io::Error>>::get(handle).expect("task should complete");
    service.shutdown();
    service.wait_termination();
}

#[test]
fn test_rayon_task_handle_cancel_during_start_race_never_reports_unsupported() {
    const QUEUED_TASKS: usize = 64;
    const ATTEMPTS: usize = 64;

    for _ in 0..ATTEMPTS {
        let service = create_single_worker_service();
        let (running, release_tx) = submit_blocking_task(&service);

        let queued_handles = (0..QUEUED_TASKS)
            .map(|_| {
                service
                    .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
                    .expect("queued task should be accepted")
            })
            .collect::<Vec<_>>();

        release_tx
            .send(())
            .expect("running task should receive release signal");
        for handle in &queued_handles {
            assert_ne!(
                handle.cancel(),
                CancelResult::Unsupported,
                "rayon task handles should keep cancellation semantics while a task is starting",
            );
        }

        running
            .get()
            .expect("running task should complete normally");
        for handle in queued_handles {
            let _ = handle.get();
        }
        service.shutdown();
        service.wait_termination();
    }
}

#[test]
fn test_rayon_task_handle_reports_status_and_try_get_states() {
    let service = create_single_worker_service();
    let (first, release_tx) = submit_blocking_task(&service);

    let queued = service
        .submit_tracked_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");
    assert_eq!(queued.status(), TaskStatus::Pending);

    let queued = match queued.try_get() {
        TryGet::Pending(queued) => queued,
        TryGet::Ready(result) => panic!("queued task should not be ready yet: {result:?}"),
    };

    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    wait_until(|| queued.is_done());
    assert_eq!(queued.status(), TaskStatus::Succeeded);

    match queued.try_get() {
        TryGet::Ready(result) => assert_eq!(result.expect("callable should complete"), 42),
        TryGet::Pending(_) => panic!("completed task should be ready"),
    }

    service.shutdown();
    service.wait_termination();
}
