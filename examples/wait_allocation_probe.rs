// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Measures allocation counts and end-to-end CPU capacity-wait costs.

use std::alloc::GlobalAlloc;
use std::alloc::Layout;
use std::alloc::System;
use std::cell::Cell;
use std::future::Future;
use std::hint::black_box;
use std::io;
use std::pin::Pin;
use std::pin::pin;
use std::process::Command;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::TaskHandle;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;
use qubit_rayon_executor::RayonExecutorService;
use tokio::runtime::Builder;
use tokio::runtime::Runtime;
use tokio::task::JoinSet;

#[path = "wait_allocation_probe/watchdog.rs"]
mod watchdog;

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::process::Stdio;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    const CHILD_MODE: &str = "QUBIT_ALLOCATION_PROBE_FIXTURE";
    const DRIVER_MODE: &str = "QUBIT_ALLOCATION_PROBE_DRIVER";

    #[test]
    fn test_allocation_row_preserves_fraction_and_total() {
        assert_eq!(
            super::allocation_row("repeated_pending_poll", 1_000, 1),
            "allocation_probe,repeated_pending_poll,each,1000,0.001000,1"
        );
        assert_eq!(
            super::allocation_row("first_pending_poll", 1, 0),
            "allocation_probe,first_pending_poll,each,1,0.000000,0"
        );
    }

    #[test]
    fn test_submission_row_preserves_elapsed_and_fractional_averages() {
        assert_eq!(
            super::submission_row("saturated_rejection", 1_000, 2_000, 3),
            "submission_probe,saturated_rejection,1000,2000,2.00,3,0.003000"
        );
    }

    #[test]
    fn test_real_child_fixture() {
        match std::env::var(CHILD_MODE).as_deref() {
            Ok("success") => {}
            Ok("failure") => std::process::exit(7),
            Ok("hang") => thread::sleep(Duration::from_secs(10)),
            _ => {}
        }
    }

    #[test]
    fn test_real_child_processes_are_supervised() {
        if std::env::var_os(DRIVER_MODE).is_some() {
            exercise_real_children();
            return;
        }

        let mut child = Command::new(std::env::current_exe().expect("test executable path"));
        child
            .args([
                "--exact",
                "tests::test_real_child_processes_are_supervised",
                "--nocapture",
            ])
            .env(DRIVER_MODE, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = child.spawn().expect("outer test driver should start");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = process.try_wait().expect("outer driver status should be available") {
                assert!(status.success(), "real child driver should pass: {status}");
                return;
            }
            if Instant::now() >= deadline {
                process.kill().expect("hung outer driver should be killed");
                let status = process.wait().expect("outer driver should be reaped");
                panic!("real child driver exceeded its independent 15 second deadline: {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn exercise_real_children() {
        use super::watchdog;

        let executable = std::env::current_exe().expect("test executable path");
        let mut success = Command::new(&executable);
        success
            .args(["--exact", "tests::test_real_child_fixture", "--nocapture"])
            .env(CHILD_MODE, "success");
        assert!(
            watchdog::run(&mut success, Duration::from_secs(2))
                .expect("success fixture should run")
                .success()
        );

        let mut failure = Command::new(&executable);
        failure
            .args(["--exact", "tests::test_real_child_fixture", "--nocapture"])
            .env(CHILD_MODE, "failure");
        let error =
            watchdog::run(&mut failure, Duration::from_secs(2)).expect_err("failure fixture should fail supervision");
        assert!(error.contains("exited unsuccessfully"));

        let mut hanging = Command::new(executable);
        hanging
            .args(["--exact", "tests::test_real_child_fixture", "--nocapture"])
            .env(CHILD_MODE, "hang");
        let error = watchdog::run(&mut hanging, Duration::from_millis(50)).expect_err("hang fixture should time out");
        assert!(error.contains("timed out"), "watchdog should explain timeout: {error}");
    }
}

const PROBE_CHILD_ARGUMENT: &str = "--probe-child";
const PROBE_BUDGET: Duration = Duration::from_secs(600);

/// Counts allocations only while a measurement window is enabled.
struct CountingAllocator;

/// Enables the allocation counter for a single measurement window.
static COUNT_ALL_ALLOCATIONS: AtomicBool = AtomicBool::new(false);

/// Stores allocation calls observed in the active measurement window.
static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

// Enables allocation counting on the current thread for isolated micro probes.
thread_local! {
    static COUNT_THREAD_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
}

/// Installs the measurement allocator for this standalone process.
#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Delegates to the system allocator and records successful allocations.
unsafe impl GlobalAlloc for CountingAllocator {
    /// Allocates memory and increments the active allocation count.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The system allocator accepts the caller-provided layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && allocation_counting_enabled() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }

    /// Releases memory through the system allocator.
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout came from the system allocator.
        unsafe { System.dealloc(pointer, layout) };
    }

    /// Resizes memory and records successful allocation growth operations.
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The pointer and layout came from the system allocator.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() && allocation_counting_enabled() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        resized
    }
}

/// Runs a closure while counting successful allocation and reallocation calls.
fn allocation_counting_enabled() -> bool {
    COUNT_ALL_ALLOCATIONS.load(Ordering::Relaxed) || COUNT_THREAD_ALLOCATIONS.try_with(Cell::get).unwrap_or(false)
}

/// Runs a closure while counting allocations from the current thread only.
fn count_thread_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    COUNT_THREAD_ALLOCATIONS.with(|count| count.set(true));
    let result = operation();
    COUNT_THREAD_ALLOCATIONS.with(|count| count.set(false));
    (result, ALLOCATION_COUNT.load(Ordering::Relaxed))
}

/// Runs a closure while counting allocations from all threads.
fn count_all_thread_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    COUNT_ALL_ALLOCATIONS.store(true, Ordering::SeqCst);
    let result = operation();
    COUNT_ALL_ALLOCATIONS.store(false, Ordering::SeqCst);
    (result, ALLOCATION_COUNT.load(Ordering::Relaxed))
}

/// Formats an allocation probe row with the average allocations per operation
/// and the integer total observed across all samples.
fn allocation_row(kind: &str, samples: usize, total: usize) -> String {
    assert!(samples > 0, "allocation samples must be nonzero");
    let average = total as f64 / samples as f64;
    format!("allocation_probe,{kind},each,{samples},{average:.6},{total}")
}

/// Formats a submission cost row with elapsed time and per-attempt averages.
fn submission_row(kind: &str, samples: usize, elapsed_ns: u128, allocations: usize) -> String {
    assert!(samples > 0, "submission samples must be nonzero");
    format!(
        "submission_probe,{kind},{samples},{elapsed_ns},{:.2},{allocations},{:.6}",
        elapsed_ns as f64 / samples as f64,
        allocations as f64 / samples as f64
    )
}

/// Supervises the measured child process or runs the measured probe child.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(PROBE_CHILD_ARGUMENT)) {
        return run_probe();
    }

    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command.arg(PROBE_CHILD_ARGUMENT);
    let status: ExitStatus = watchdog::run(&mut command, PROBE_BUDGET).map_err(std::io::Error::other)?;
    if !status.success() {
        return Err(std::io::Error::other(format!("probe child exited unsuccessfully: {status}")).into());
    }
    Ok(())
}

/// Runs the allocation and contention probes in a warmed Tokio runtime.
fn run_probe() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    print_environment();
    measure_wait_allocations()?;
    measure_saturated_rejections()?;
    measure_contention(&runtime)?;
    Ok(())
}

/// Prints machine and runtime details needed to interpret recorded samples.
fn print_environment() {
    let parallelism = thread::available_parallelism().map_or(0, usize::from);
    let rustc = Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".to_owned());
    println!(
        "metadata,os={},arch={},available_parallelism={parallelism}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("metadata,rustc={rustc},minimum_rust={}", env!("CARGO_PKG_RUST_VERSION"));
    println!(
        "metadata,rayon_executor_source={},executor_source={}/../rs-executor",
        env!("CARGO_MANIFEST_DIR"),
        env!("CARGO_MANIFEST_DIR")
    );
    println!("metadata,rayon_workers=4,capacity=16,rounds=5,warmup_rounds=2,tasks_per_producer=1000");
    println!("allocation_probe,kind,factor,samples,allocations,total_allocations");
    println!(
        "throughput_probe,kind,producers,work_units,round,tasks,elapsed_ns,tasks_per_second,p50_submit_ns,p95_submit_ns"
    );
    println!("submission_probe,kind,samples,elapsed_ns,ns_per_attempt,allocations,allocations_per_attempt");
}

/// Separates changed-future creation, first and repeated pending polls, and
/// notification costs.
fn measure_wait_allocations() -> Result<(), Box<dyn std::error::Error>> {
    const SAMPLES: usize = 1_000;
    eprintln!("probe_progress,stage=wait_allocations");
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()?;
    let mut changes = service.capacity_changes();
    for _ in 0..32 {
        drop(changes.changed());
    }
    let noop_waker = Waker::noop();
    {
        let mut warmup_context = Context::from_waker(noop_waker);
        let mut warmup = pin!(changes.changed());
        for _ in 0..32 {
            assert!(matches!(
                Future::poll(Pin::as_mut(&mut warmup), &mut warmup_context),
                Poll::Pending
            ));
        }
    }
    eprintln!("probe_progress,stage=future_creation");
    let (_, creation_allocations) = count_thread_allocations(|| {
        for _ in 0..SAMPLES {
            drop(changes.changed());
        }
    });
    println!("{}", allocation_row("future_creation", SAMPLES, creation_allocations));

    let (first_poll_allocations, poll_allocations) = {
        let mut pending = pin!(changes.changed());
        let mut context = Context::from_waker(noop_waker);
        eprintln!("probe_progress,stage=first_pending_poll");
        let (_, first) = count_thread_allocations(|| {
            assert!(matches!(
                Future::poll(Pin::as_mut(&mut pending), &mut context),
                Poll::Pending
            ));
        });
        eprintln!("probe_progress,stage=repeated_pending_poll");
        let (_, repeated) = count_thread_allocations(|| {
            for _ in 0..SAMPLES {
                assert!(matches!(
                    Future::poll(Pin::as_mut(&mut pending), &mut context),
                    Poll::Pending
                ));
            }
        });
        (first, repeated)
    };
    println!("{}", allocation_row("first_pending_poll", 1, first_poll_allocations));
    println!("{}", allocation_row("repeated_pending_poll", SAMPLES, poll_allocations));
    for allocations in measure_notification_allocations(&service)? {
        println!("{}", allocation_row("capacity_notification", 1, allocations));
    }
    service.shutdown();
    Ok(())
}

/// Measures saturated rejection/recovery separately from successful admission.
fn measure_saturated_rejections() -> Result<(), Box<dyn std::error::Error>> {
    const SAMPLES: usize = 1_000;
    eprintln!("probe_progress,stage=saturated_rejection");
    let service = RayonExecutorService::builder()
        .num_threads(1)
        .task_capacity(1)
        .build()?;
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = service
        .try_submit_callable(move || {
            started_tx.send(()).expect("report blocker start");
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("release blocker");
            Ok::<(), io::Error>(())
        })
        .map_err(|rejected| rejected.into_parts().0)?;
    if let Err(error) = started_rx.recv_timeout(Duration::from_secs(10)) {
        drop(release_tx);
        let _ = blocker.get();
        return Err(error.into());
    }

    let warmup_callable = || Ok::<(), io::Error>(());
    match service.try_submit_callable(warmup_callable) {
        Err(rejected) => {
            let (reason, callable) = rejected.into_parts();
            assert_eq!(reason, SubmissionError::Saturated);
            let _ = callable;
        }
        Ok(handle) => {
            drop(handle);
            panic!("warmup submission unexpectedly succeeded while blocker occupied capacity");
        }
    }

    // Reuse the callable recovered from every rejection. Invoke it after the
    // measurement window to verify it remains intact without skewing the cost.
    let invoked = Arc::new(AtomicUsize::new(0));
    let invocation_marker = Arc::clone(&invoked);
    let mut callable = Some(move || {
        invocation_marker.fetch_add(1, Ordering::Relaxed);
        Ok::<(), io::Error>(())
    });
    let (rejection_elapsed_ns, rejection_allocations) = count_thread_allocations(|| {
        let started = Instant::now();
        for _ in 0..SAMPLES {
            let attempt = callable.take().expect("rejected callable is recovered");
            match service.try_submit_callable(attempt) {
                Err(rejected) => {
                    let (reason, returned) = rejected.into_parts();
                    assert_eq!(reason, SubmissionError::Saturated);
                    callable = Some(returned);
                }
                Ok(handle) => {
                    drop(handle);
                    panic!("submission unexpectedly succeeded while blocker occupied capacity");
                }
            }
        }
        started.elapsed().as_nanos()
    });
    let returned = callable.take().expect("last rejected callable is recovered");
    assert_eq!(invoked.load(Ordering::Relaxed), 0, "rejected callable must not run");
    let _ = release_tx.send(());
    blocker.get()?;
    returned()?;
    assert_eq!(
        invoked.load(Ordering::Relaxed),
        1,
        "recovered callable should remain callable"
    );
    wait_for_accounting_release(&service, Duration::from_secs(10))?;

    // The service is free again; this one-attempt control measures successful
    // admission on the submitting thread, excluding task completion.
    let accepted_callable = || Ok::<(), io::Error>(());
    let ((accepted_elapsed_ns, accepted_result), accepted_allocations) = count_thread_allocations(|| {
        let started = Instant::now();
        let result = service.try_submit_callable(accepted_callable);
        (started.elapsed().as_nanos(), result)
    });
    let accepted_handle = accepted_result.map_err(|rejected| rejected.into_parts().0)?;
    accepted_handle.get()?;

    println!(
        "{}",
        submission_row(
            "saturated_rejection",
            SAMPLES,
            rejection_elapsed_ns,
            rejection_allocations
        )
    );
    println!(
        "{}",
        submission_row("successful_admission", 1, accepted_elapsed_ns, accepted_allocations)
    );
    service.shutdown();
    Ok(())
}

/// Waits for service-side accounting to release completed work.
fn wait_for_accounting_release(service: &RayonExecutorService, timeout: Duration) -> Result<(), io::Error> {
    let deadline = Instant::now() + timeout;
    loop {
        let stats = service.stats();
        if stats.queued == 0 && stats.running == 0 && stats.cancelling == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("service accounting did not reach zero before the deadline: {stats:?}"),
            ));
        }
        thread::yield_now();
    }
}

/// Measures allocations from task completion through observing a capacity
/// notification.
fn measure_notification_allocations(service: &RayonExecutorService) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let mut samples = Vec::with_capacity(5);
    for round in 1..=5 {
        eprintln!("probe_progress,stage=capacity_notification,round={round}");
        let mut changes = service.capacity_changes();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let handle = service
            .try_submit_callable(move || {
                started_tx.send(()).expect("report blocker start");
                release_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release blocker");
                Ok::<(), io::Error>(())
            })
            .map_err(|rejected| rejected.into_parts().0)?;
        started_rx.recv_timeout(Duration::from_secs(10))?;
        let mut changed = pin!(changes.changed());
        let noop_waker = Waker::noop();
        let mut context = Context::from_waker(noop_waker);
        assert!(matches!(
            Future::poll(Pin::as_mut(&mut changed), &mut context),
            Poll::Pending
        ));

        let (_, allocations) = count_all_thread_allocations(|| {
            release_tx.send(()).expect("release task blocker");
            handle.get().expect("blocking task should finish");
            loop {
                if Future::poll(Pin::as_mut(&mut changed), &mut context).is_ready() {
                    break;
                }
                thread::yield_now();
            }
        });
        samples.push(allocations);
    }
    Ok(samples)
}

/// Runs the multi-producer capacity-wait matrix and prints raw trial samples.
fn measure_contention(runtime: &Runtime) -> Result<(), Box<dyn std::error::Error>> {
    for producers in [1, 8, 32] {
        for (label, work_units) in [("short", 64), ("long", 4_000)] {
            run_contention_case(runtime, producers, label, work_units, 0)?;
            run_contention_case(runtime, producers, label, work_units, -1)?;
            for round in 1..=5 {
                run_contention_case(runtime, producers, label, work_units, round)?;
            }
        }
    }
    Ok(())
}

/// Measures one warmup or raw throughput trial for a producer/workload pair.
fn run_contention_case(
    runtime: &Runtime,
    producers: usize,
    work_label: &str,
    work_units: u64,
    round: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    const TASKS_PER_PRODUCER: usize = 1_000;
    eprintln!("probe_progress,stage=contention,producers={producers},work_units={work_units},round={round}");
    let service = Arc::new(
        RayonExecutorService::builder()
            .num_threads(4)
            .task_capacity(16)
            .build()?,
    );
    let started = Instant::now();
    let latencies = runtime.block_on(async {
        let mut producers_set = JoinSet::new();
        for _ in 0..producers {
            let service = Arc::clone(&service);
            producers_set.spawn(async move {
                let mut samples = Vec::with_capacity(TASKS_PER_PRODUCER);
                for _ in 0..TASKS_PER_PRODUCER {
                    let submit_started = Instant::now();
                    let callable = move || Ok::<u64, io::Error>(cpu_work(work_units));
                    let handle = submit_after_capacity(&service, callable).await?;
                    drop(handle);
                    samples.push(submit_started.elapsed().as_nanos());
                }
                Ok::<Vec<u128>, SubmissionError>(samples)
            });
        }

        let mut samples = Vec::with_capacity(producers * TASKS_PER_PRODUCER);
        while let Some(result) = producers_set.join_next().await {
            samples.extend(result??);
        }
        Ok::<Vec<u128>, Box<dyn std::error::Error>>(samples)
    })?;
    service.shutdown();
    runtime.block_on(service.await_termination());
    let elapsed_ns = started.elapsed().as_nanos();
    let p50 = percentile(&latencies, 50);
    let p95 = percentile(&latencies, 95);
    let task_count = producers * TASKS_PER_PRODUCER;
    let throughput = task_count as f64 / (elapsed_ns as f64 / 1_000_000_000.0);
    println!(
        "throughput_probe,{work_label},{producers},{work_units},{round},{task_count},{elapsed_ns},{throughput:.2},{p50},{p95}"
    );
    Ok(())
}

/// Submits a one-shot callable and retries the same callable after saturation.
async fn submit_after_capacity<C>(
    service: &RayonExecutorService,
    task: C,
) -> Result<TaskHandle<u64, io::Error>, SubmissionError>
where
    C: FnOnce() -> Result<u64, io::Error> + Send + 'static,
{
    let mut task = Some(task);
    let mut callable = Some(move || task.take().expect("probe task runs once")());
    let mut changes = service.capacity_changes();
    loop {
        let attempt = callable
            .take()
            .expect("probe callable remains available until accepted");
        match service.try_submit_callable(attempt) {
            Ok(handle) => return Ok(handle),
            Err(rejected) => {
                let (reason, rejected_callable) = rejected.into_parts();
                callable = Some(rejected_callable);
                if reason == SubmissionError::Saturated {
                    changes.changed().await.map_err(|_| SubmissionError::Shutdown)?;
                } else {
                    return Err(reason);
                }
            }
        }
    }
}

/// Performs a deterministic CPU loop whose result remains observable.
fn cpu_work(work_units: u64) -> u64 {
    let mut value = 0_u64;
    for index in 0..work_units {
        value = value
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(index ^ 1_442_695_040_888_963_407);
    }
    black_box(value)
}

/// Returns the nearest-rank percentile from elapsed-nanosecond samples.
fn percentile(samples: &[u128], percent: usize) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() - 1) * percent / 100]
}
