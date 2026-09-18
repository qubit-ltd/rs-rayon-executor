# Qubit Rayon Executor User Guide

[中文版](user_guide.zh_CN.md) | Applies to `qubit-rayon-executor` 0.7.0 and Rust 1.94+

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
best-effort cancellation before work starts.

```
caller -> bounded accepted-task capacity -> Rayon pool -> task handle
                                      \-> Saturated when capacity is full
```

## Scenario: Isolate a CPU Aggregation

An application must aggregate values without competing with its other execution
domains. Success means receiving the total and then cleanly ending admission.

### Installation and Minimal Configuration

The package has `publish = false`, so use it from the Qubit source tree or
workspace. The submission trait is provided by `qubit-executor`:

```toml
[dependencies]
qubit-executor = "0.8"
qubit-rayon-executor = { version = "0.7", path = "../rs-rayon-executor" }
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
    let handle = service.submit_callable(move || Ok::<usize, io::Error>(values.into_iter().sum()))?;
    let total = handle.get()?;
    service.shutdown();
    service.wait_termination();
    Ok(total)
}

assert_eq!(aggregate()?, 76);
```

Successful submission means that the service accepted the task, not that the
callable succeeded. Inspect the `TaskHandle` result for a task error or panic.
`shutdown` finishes accepted work, while `wait_termination` waits for it to
reach a terminal state.

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
```

Call `stop` to abandon queued work. Its `StopReport` records queued, running,
and cancelled tasks, but it cannot forcibly interrupt a running Rayon task.

## Errors and Diagnostics

| Observation | Meaning and response |
| --- | --- |
| `ZeroThreadCount`, `ZeroStackSize`, or `ZeroTaskCapacity` | A builder value was zero; configure a non-zero value. |
| `SubmissionError::Saturated` | Accepted unfinished work reached `task_capacity`; reduce pressure, collect results, cancel queued tracked work, or choose a suitable capacity. |
| `SubmissionError::Shutdown` | The service no longer admits tasks; submit new work only to a running service. |
| `TaskExecutionError::Cancelled` | A queued tracked task was cancelled; resubmit only if the application still needs it. |

Task failures and panics appear through `get` or `.await`, rather than through
successful submission.

## Troubleshooting

Capacity includes queued, running, and cancellation-in-progress tasks, not just
visible queue items. If `wait_termination` does not return, call `shutdown` or
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
