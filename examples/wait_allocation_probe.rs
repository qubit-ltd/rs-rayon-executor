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
use std::future::Future;
use std::hint::black_box;
use std::io;
use std::pin::Pin;
use std::pin::pin;
use std::process::Command;
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

/// Runs the allocation and contention probes in a warmed Tokio runtime.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    print_environment();
    measure_wait_allocations()?;
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
}

/// Separates changed-future creation, repeated pending polls, and notification
/// costs.
fn measure_wait_allocations() -> Result<(), Box<dyn std::error::Error>> {
    const SAMPLES: usize = 1_000;
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
    let (_, creation_allocations) = count_thread_allocations(|| {
        for _ in 0..SAMPLES {
            drop(changes.changed());
        }
    });
    println!(
        "allocation_probe,future_creation,each,{SAMPLES},{},{}",
        creation_allocations / SAMPLES,
        creation_allocations
    );

    let (_, poll_allocations) = {
        let mut pending = pin!(changes.changed());
        let mut context = Context::from_waker(noop_waker);
        count_thread_allocations(|| {
            for _ in 0..SAMPLES {
                assert!(matches!(
                    Future::poll(Pin::as_mut(&mut pending), &mut context),
                    Poll::Pending
                ));
            }
        })
    };
    println!(
        "allocation_probe,repeated_pending_poll,each,{SAMPLES},{},{}",
        poll_allocations / SAMPLES,
        poll_allocations
    );
    for allocations in measure_notification_allocations(&service)? {
        println!(
            "allocation_probe,capacity_notification,each,1,{},{}",
            allocations, allocations
        );
    }
    service.shutdown();
    Ok(())
}

/// Measures allocations from task completion through observing a capacity
/// notification.
fn measure_notification_allocations(service: &RayonExecutorService) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let mut samples = Vec::with_capacity(5);
    for _ in 0..5 {
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
use std::cell::Cell;
