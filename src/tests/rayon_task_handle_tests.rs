// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use qubit_executor::CancelResult;
use qubit_executor::TaskStatus;
use qubit_executor::task::spi::TaskEndpointPair;

use crate::internal::CallableJob;
use crate::rayon_executor_service_state::RayonExecutorServiceState;
use crate::rayon_task_handle::RayonTaskHandle;

fn assert_terminal_cancellation_classification<F>(callable: F, expected: TaskStatus)
where
    F: FnOnce() -> Result<usize, io::Error> + Send + 'static,
{
    let state = RayonExecutorServiceState::new(1, 1);
    let (tracked, slot) = TaskEndpointPair::<usize, io::Error>::new().into_tracked_parts();
    let mut callable = Some(callable);
    let job = CallableJob::new(move || callable.take().expect("callable should run once")(), slot);
    let admission = match state.admit(Box::new(job)) {
        Ok(admission) => admission,
        Err(_) => panic!("task should be admitted"),
    };
    let handle = RayonTaskHandle::new(tracked, admission.task_id, Arc::clone(&state));
    let cancellation = handle.cancellation_handle();
    let (task_id, job) = state.take_next().expect("worker should take admitted task");

    job.run();
    assert_eq!(handle.status(), expected);
    assert_eq!(state.stats().running, 1);
    assert_eq!(handle.cancel(), CancelResult::AlreadyFinished);
    assert_eq!(cancellation.cancel(), CancelResult::AlreadyFinished);
    assert!(!state.finish_running(task_id));
    assert_eq!(state.stats().running, 0);
}

#[test]
fn test_cancellation_endpoint_reports_finished_before_worker_accounting() {
    assert_terminal_cancellation_classification(|| Ok(42), TaskStatus::Succeeded);
    assert_terminal_cancellation_classification(|| Err(io::Error::other("task failure")), TaskStatus::Failed);
    assert_terminal_cancellation_classification(
        || -> Result<usize, io::Error> { panic!("task panic") },
        TaskStatus::Panicked,
    );
}

#[test]
fn test_cancellation_endpoint_cannot_cancel_after_worker_takes_job_before_start() {
    let state = RayonExecutorServiceState::new(1, 1);
    let (tracked, slot) = TaskEndpointPair::<usize, Infallible>::new().into_tracked_parts();
    let ran = Arc::new(AtomicBool::new(false));
    let callable_ran = Arc::clone(&ran);
    let job = CallableJob::new(
        move || {
            callable_ran.store(true, Ordering::SeqCst);
            Ok(42)
        },
        slot,
    );
    let admission = match state.admit(Box::new(job)) {
        Ok(admission) => admission,
        Err(_) => panic!("task should be admitted"),
    };
    let handle = RayonTaskHandle::new(tracked, admission.task_id, Arc::clone(&state));
    let cancellation = handle.cancellation_handle();
    let (task_id, job) = state.take_next().expect("worker should take admitted task");

    assert_eq!(handle.status(), TaskStatus::Pending);
    assert_eq!(cancellation.cancel(), CancelResult::AlreadyRunning);
    job.run();
    assert!(!state.finish_running(task_id));

    assert!(ran.load(Ordering::SeqCst));
    assert_eq!(handle.status(), TaskStatus::Succeeded);
    assert_eq!(cancellation.cancel(), CancelResult::AlreadyFinished);
    assert_eq!(state.stats().queued, 0);
    assert_eq!(state.stats().running, 0);
    assert_eq!(state.stats().cancelling, 0);
}
