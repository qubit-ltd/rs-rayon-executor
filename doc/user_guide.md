# Qubit Rayon Executor User Guide

[中文版](user_guide.zh_CN.md) | Applies to `qubit-rayon-executor` 0.9 and Rust 1.94+

## Purpose and Audience

This guide is for Rust developers who need to run synchronous CPU-bound tasks
in an isolated Rayon pool while retaining Qubit's `ExecutorService` lifecycle
and task-handle APIs. It does not make blocking IO or async futures CPU work;
use a thread-pool or Tokio executor for those workloads.

## Conceptual Model

`RayonExecutorServiceBuilder` creates a dedicated Rayon pool and
`RayonExecutorService` accepts work through `ExecutorService`. Each accepted
task is queued, running, or being cancelled; all three states consume
`task_capacity`, whose default is 1024. Use `submit` for detached runnables,
`submit_callable` for a result, and tracked submissions for status or
best-effort cancellation before work starts. Detached submissions expose only
admission status; see [Errors and Diagnostics](#errors-and-diagnostics) for
where their final task result goes.

```text
caller -> bounded accepted-task capacity -> Rayon pool -> task handle
                                      \-> Saturated when capacity is full
```

## Scenario: Isolate a CPU Aggregation

An application must aggregate values without competing with its other execution
domains. Success means receiving the total and then cleanly ending admission.

### Installation and Minimal Configuration

Add both crates from crates.io. The submission trait is provided by
`qubit-executor`:

```toml
[dependencies]
qubit-executor = "0.9"
qubit-rayon-executor = "0.9"
```

### Core Workflow

Configure the isolated pool, submit the calculation, observe its result, then
shut down admissions:

```rust
use std::io;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

fn aggregate() -> Result<usize, Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder()
        .num_threads(4)
        .task_capacity(1024)
        .thread_name_prefix("cpu-worker")
        .build()?;
    let values = vec![8_usize, 13, 21, 34];
    let handle = service.submit_callable(move || Ok::<usize, io::Error>(values.iter().copied().sum()))?;
    let total = handle.get()?;
    service.shutdown();
    service.wait_termination();
    Ok(total)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(aggregate()?, 76);
    Ok(())
}
```

Successful submission means that the service accepted the task, not that the
callable succeeded. Inspect the `TaskHandle` result for a task error or panic.
`shutdown` finishes accepted work, while `wait_termination` waits for it to
reach a terminal state.

Submitting a task consumes its callable even when submission returns
`Saturated` or `Shutdown`. Keep the inputs needed to retry outside the closure,
then build a fresh callable for each attempt. For one-shot task values, keep
shared ownership until submission succeeds or use a retry API designed to
retain the task.

## Async shutdown and capacity waits

Async waiting is optional. Enable the crate feature to use
`await_termination` and `capacity_changes`. The wait can be driven by any Rust
future executor; it does not require a Tokio runtime. The example below uses
Tokio APIs and therefore requires a Tokio runtime:

```toml
[dependencies]
qubit-executor = "0.9"
qubit-rayon-executor = { version = "0.9", features = ["async-wait"] }
tokio = { version = "1.54", features = ["macros", "rt-multi-thread"] }
```

In async code, prefer `await_termination` after `shutdown` or `stop`. The
synchronous `wait_termination` blocks its caller and should not run on a Tokio
runtime worker. This only makes the termination wait non-blocking: `stop` and
a successful queued-task `cancel` run captured-value destructors synchronously
on their caller's thread, and their duration depends on user cleanup. Such work
remains counted as `cancelling` until cleanup finishes, even if the cancellation
result is already available. Do not let cleanup synchronously wait for
termination that depends on that same cleanup.

To keep potentially slow `stop` cleanup off a Tokio worker, run it in
`spawn_blocking`, then await termination:

```rust
use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

async fn stop_without_blocking_runtime(
    service: &RayonExecutorService,
) -> Result<(), tokio::task::JoinError> {
    let stopping = service.clone();
    let _report = tokio::task::spawn_blocking(move || stopping.stop()).await?;
    service.await_termination().await;
    Ok(())
}
```

This example requires the `async-wait` feature and a running Tokio runtime
(the dependency configuration above enables Tokio's `rt-multi-thread`). The
`?` propagates a `JoinError` if the blocking task panics or is cancelled before
starting. Dropping the join future does not forcibly terminate a blocking
closure that has already started. `stop` still cannot interrupt running CPU
tasks; if one does not return, the termination wait can continue indefinitely.

For backpressure, subscribe before submitting. After `Saturated`, wait for a
change and retry with a new callable. Notifications are hints: another
producer may take the capacity first, and the API makes no FIFO, fairness, or
finite-wait guarantee. Handle `Shutdown` and a closed notification channel as
terminal outcomes:

```rust
use std::io;

use qubit_executor::service::{ExecutorService, SubmissionError};
use qubit_rayon_executor::RayonExecutorService;

async fn submit_after_capacity(
    service: &RayonExecutorService,
    values: Vec<usize>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut changes = service.capacity_changes();
    let handle = loop {
        let task_values = values.clone();
        match service.submit_callable(move || {
            Ok::<usize, io::Error>(task_values.iter().copied().sum())
        }) {
            Ok(handle) => break handle,
            Err(SubmissionError::Saturated) => {
                if changes.changed().await.is_err() {
                    return Err("service notification channel closed".into());
                }
            }
            Err(SubmissionError::Shutdown) => return Err("service shut down".into()),
            Err(error) => return Err(error.into()),
        }
    };
    Ok(handle.await?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder().num_threads(2).task_capacity(8).build()?;
    let total = submit_after_capacity(&service, vec![8, 13, 21, 34]).await?;
    assert_eq!(total, 76);
    service.shutdown();
    service.await_termination().await;
    Ok(())
}
```

`stats()` returns a consistent snapshot sampled under the service state lock.
Its `queued`, `running`, and `cancelling` counts all consume capacity.
`cancelling` includes cleanup still in progress after a task is removed from
the queue. A result becoming available and a cancellation cleanup finishing
can occur at slightly different times; observing or collecting a result does
not itself release capacity.

## Advanced Usage

Tracked handles support status and cancellation. Cancellation succeeds only
while a task is queued; a running task reports `AlreadyRunning`, and a completed
task reports `AlreadyFinished`.

```rust
use std::io;
use std::sync::mpsc;

use qubit_executor::CancelResult;
use qubit_executor::TaskExecutionError;
use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder().num_threads(1).task_capacity(2).build()?;
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = service.submit_tracked(move || {
        started_tx.send(()).expect("record task start");
        release_rx.recv().map_err(|error| io::Error::other(error.to_string()))?;
        Ok::<(), io::Error>(())
    })?;
    started_rx.recv()?;
    let queued = service.submit_tracked_callable(|| Ok::<(), io::Error>(()))?;
    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(matches!(queued.get(), Err(TaskExecutionError::Cancelled)));
    release_tx.send(())?;
    running.get()?;
    service.shutdown();
    service.wait_termination();
    Ok(())
}
```

Call `stop` to abandon queued work. Its `StopReport` records queued, running,
and cancelled tasks, but it cannot forcibly interrupt a running Rayon task.

## Errors and Diagnostics

| Observation | Meaning and response |
| --- | --- |
| `ZeroThreadCount`, `ZeroStackSize`, or `ZeroTaskCapacity` | A builder value was zero; configure a non-zero value. |
| `SubmissionError::Saturated` | Accepted unfinished work reached `task_capacity`; wait for a capacity notification and retry with a newly constructed callable, reduce pressure, cancel queued tracked work, or choose a suitable capacity. |
| `SubmissionError::Shutdown` | The service no longer admits tasks; submit new work only to a running service. |
| `TaskExecutionError::Cancelled` | A queued tracked task was cancelled; resubmit only if the application still needs it. |

For submissions that return a result handle, task failures and panics are
observed through `get` or `.await`. Detached `submit` returns only admission
status and discards the final task result. Use a callable submission when the
caller needs to observe failure, or report failures from the detached task
itself.

## Troubleshooting

Capacity includes queued, running, and cancellation-in-progress tasks, not just
visible queue items. Collecting a result does not release capacity; task
completion or cancellation cleanup does. If `wait_termination` does not return, call `shutdown` or
`stop` and check for blocked accepted tasks. Do not synchronously wait for a
second task from the same saturated pool; use Rayon `join`/`scope` or async
coordination. For long-running CPU work needing interruption, implement
cooperative cancellation in the task; this service does not force-stop it.

## Limitations and Best Practices

Use this service only for synchronous CPU-bound work. Long blocking IO reduces
the pool's available parallelism, and async futures belong to an async executor.
Keep captures bounded, treat `task_capacity` as backpressure rather than memory
storage, and always finish admissions with `shutdown` or `stop`.

## Further Reading

- [README](../README.md)
- [中文用户手册](user_guide.zh_CN.md)
- [API documentation](https://docs.rs/qubit-rayon-executor)
