/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! Tests for [`RayonExecutorService`](qubit_rayon_executor::service::RayonExecutorService).

use std::{
    io,
    sync::mpsc,
    time::Duration,
};

use qubit_executor::TaskExecutionError;
use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
};
use qubit_rayon_executor::{
    RayonExecutorService,
    RayonExecutorServiceBuildError,
};

fn ok_unit_task() -> Result<(), io::Error> {
    Ok(())
}

fn ok_usize_task() -> Result<usize, io::Error> {
    Ok(42)
}

fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime for rayon executor service tests")
}

fn create_single_worker_service() -> RayonExecutorService {
    RayonExecutorService::builder()
        .num_threads(1)
        .build()
        .expect("rayon executor service should be created")
}

fn wait_started(receiver: mpsc::Receiver<()>) {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("task should start within timeout");
}

fn wait_until<F>(mut condition: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "condition should become true within timeout");
}

#[test]
fn test_rayon_executor_service_submit_acceptance_is_not_task_success() {
    let service = RayonExecutorService::new().expect("service should be created");

    service
        .submit(ok_unit_task as fn() -> Result<(), io::Error>)
        .expect("service should accept shared runnable")
        .get()
        .expect("shared runnable should complete successfully");

    let handle = service
        .submit(|| Err::<(), _>(io::Error::other("task failed")))
        .expect("service should accept runnable");

    let err = handle
        .get()
        .expect_err("accepted runnable should report task failure through handle");
    assert!(matches!(err, TaskExecutionError::Failed(_)));
    service.shutdown();
    create_runtime().block_on(service.await_termination());
}

#[test]
fn test_rayon_executor_service_submit_callable_returns_value() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    assert_eq!(
        handle.get().expect("callable should complete successfully"),
        42,
    );
    service.shutdown();
    create_runtime().block_on(service.await_termination());
}

#[tokio::test]
async fn test_rayon_task_handle_can_be_awaited() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    assert_eq!(handle.await.expect("handle should await result"), 42);
    service.shutdown();
    service.await_termination().await;
}

#[test]
fn test_rayon_executor_service_shutdown_rejects_new_tasks() {
    let service = RayonExecutorService::new().expect("service should be created");

    service.shutdown();
    let result = service.submit(ok_unit_task as fn() -> Result<(), io::Error>);

    assert!(matches!(result, Err(RejectedExecution::Shutdown)));
    create_runtime().block_on(service.await_termination());
    assert!(service.is_shutdown());
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_executor_service_shutdown_now_cancels_queued_tasks() {
    let service = create_single_worker_service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = service
        .submit(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should be accepted");
    wait_started(started_rx);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    let report = service.shutdown_now();

    assert_eq!(report.queued, 1);
    assert_eq!(report.running, 1);
    assert_eq!(report.cancelled, 1);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    create_runtime().block_on(service.await_termination());
    assert!(service.is_terminated());
}

#[test]
fn test_rayon_task_handle_cancel_before_start_reports_cancelled() {
    let service = create_single_worker_service();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first = service
        .submit(move || {
            started_tx
                .send(())
                .expect("test should receive task start signal");
            release_rx
                .recv()
                .map_err(|err| io::Error::other(err.to_string()))?;
            Ok::<(), io::Error>(())
        })
        .expect("first task should be accepted");
    wait_started(started_rx);
    let queued = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("queued task should be accepted");

    assert!(queued.cancel());
    assert!(queued.is_done());
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    service.shutdown();
    release_tx
        .send(())
        .expect("blocking task should receive release signal");
    first.get().expect("running task should complete normally");
    create_runtime().block_on(service.await_termination());
}

#[test]
fn test_rayon_task_handle_reports_panicked_task() {
    let service = RayonExecutorService::new().expect("service should be created");

    let handle = service
        .submit(|| -> Result<(), io::Error> { panic!("rayon service panic") })
        .expect("service should accept panicking task");

    assert!(matches!(handle.get(), Err(TaskExecutionError::Panicked)));
    service.shutdown();
    create_runtime().block_on(service.await_termination());
}

#[test]
fn test_rayon_executor_service_builder_validates_configuration() {
    let zero_threads = RayonExecutorService::builder().num_threads(0).build();
    assert!(matches!(
        zero_threads,
        Err(RayonExecutorServiceBuildError::ZeroThreadCount),
    ));

    let zero_stack = RayonExecutorService::builder()
        .num_threads(1)
        .stack_size(0)
        .build();
    assert!(matches!(
        zero_stack,
        Err(RayonExecutorServiceBuildError::ZeroStackSize),
    ));
}

#[test]
fn test_rayon_executor_service_builder_sets_thread_options() {
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .thread_name_prefix("rayon-custom")
        .stack_size(2 * 1024 * 1024)
        .build()
        .expect("service should be created with custom thread options");

    let handle = service
        .submit_callable(|| {
            Ok::<_, io::Error>(
                std::thread::current()
                    .name()
                    .expect("rayon worker should be named")
                    .to_owned(),
            )
        })
        .expect("service should accept callable");

    assert!(
        handle
            .get()
            .expect("callable should return worker name")
            .starts_with("rayon-custom-")
    );
    service.shutdown();
    create_runtime().block_on(service.await_termination());
}

#[test]
fn test_rayon_task_handle_cancel_after_completion_returns_false() {
    let service = create_single_worker_service();
    let handle = service
        .submit_callable(ok_usize_task as fn() -> Result<usize, io::Error>)
        .expect("service should accept callable");

    wait_until(|| handle.is_done());
    assert!(!handle.cancel());
    assert_eq!(handle.get().expect("callable should complete"), 42);
    service.shutdown();
    create_runtime().block_on(service.await_termination());
}

#[tokio::test]
async fn test_rayon_executor_service_await_termination_waits_before_shutdown() {
    let service = RayonExecutorService::new().expect("service should be created");
    let waiter_service = service.clone();
    let waiter = tokio::spawn(async move {
        waiter_service.await_termination().await;
    });

    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    service.shutdown();
    waiter
        .await
        .expect("termination waiter should finish after shutdown");
}
