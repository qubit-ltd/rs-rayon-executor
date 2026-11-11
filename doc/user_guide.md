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
where their final task result goes. If internal unique task identifiers are
exhausted, the service stops accepting new work and returns `Shutdown`;
already accepted work continues to completion.

```text
caller -> bounded accepted-task capacity -> Rayon pool -> task handle
                                      \-> Saturated when capacity is full
```

Clones share the same pool and lifecycle. Calling `shutdown` or `stop` through
any clone ends admission for all clones. Dropping a clone does not implicitly
shut down, stop, or join the service. When completion must be guaranteed,
coordinate an explicit `shutdown` or `stop` and wait for termination.

An accepted task must not synchronously wait for its own service to terminate.
Termination includes every accepted task finishing, so the task and the
termination wait depend on each other. Idle workers do not break this cycle,
and calling `shutdown` first does not break it either. Coordinate task results
and service termination outside the task.

## Scenario: Isolate a CPU Aggregation

An application must aggregate values without competing with its other execution
domains. Success means receiving the total and then cleanly ending admission.

### Installation and Minimal Configuration

Add both crates from crates.io. The submission trait is provided by
`qubit-executor`:

```toml
[dependencies]
qubit-executor = "0.9.0"
qubit-rayon-executor = "0.9"
```

The examples below can be checked against the current source checkout with
`cargo test --test documentation_examples_tests`. To check the published
crates.io combination, run
`cargo test --test documentation_examples_tests test_registry_documentation_examples_compile_and_run -- --ignored --exact --nocapture`
from the repository root. This explicit smoke test needs registry network
access and uses its own fixture, lockfile, and build directory. It reports the
exact resolved crate versions and features: Cargo selects the latest compatible
published versions allowed by the manifest, so the result applies to those
versions only. An unpublished candidate cannot be verified by this registry
test; use the source tests and package checks for the current checkout.

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
`shutdown` stops admission and allows accepted work to continue; it does not
wait for completion. `wait_termination` waits until admission has stopped and
all accepted tasks' service-side execution or cancellation accounting has been
released. Results may already be terminal while that accounting remains
occupied, including during cancellation cleanup. Termination does not
guarantee that idle scheduled closures have exited or that Rayon worker
threads have been joined.

The `ExecutorService::submit_callable` method consumes its callable even when
submission returns `Saturated` or `Shutdown`. For one-shot task values, use the
inherent `RayonExecutorService::try_submit_callable` method: it returns the same
callable inside `RejectedTask<C>` if admission fails. Call `into_parts()` to
recover the reason and callable. Dropping the rejected value drops the callable
normally. `RejectedTask` is this crate's type and is distinct from the similarly
named Tokio executor type.

## Async shutdown and capacity waits

Async waiting is optional. Enable the crate feature to use
`await_termination` and `capacity_changes`. The wait can be driven by any Rust
future executor; it does not require a Tokio runtime. The example below uses
Tokio APIs and therefore requires a Tokio runtime:

```toml
[dependencies]
qubit-executor = "0.9.0"
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
change and retry the callable returned by `RejectedTask`. Notifications are
hints: another producer may take the capacity first, and the API makes no FIFO,
fairness, or finite-wait guarantee. Handle `Shutdown` and a closed
notification channel as terminal outcomes:

```rust
use std::io;
use std::error::Error;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

fn sum_once(values: Box<[usize]>) -> impl FnMut() -> Result<usize, io::Error> + Send + 'static {
    let mut values = Some(values);
    move || Ok(values.take().expect("callable runs at most once").iter().copied().sum())
}

async fn submit_after_capacity(
    service: &RayonExecutorService,
    values: Box<[usize]>,
) -> Result<usize, Box<dyn Error>> {
    let mut changes = service.capacity_changes();
    let mut task = sum_once(values);
    let handle = loop {
        match service.try_submit_callable(task) {
            Ok(handle) => break handle,
            Err(rejected) => {
                let (reason, returned_task) = rejected.into_parts();
                match reason {
                    qubit_executor::service::SubmissionError::Saturated => {
                        task = returned_task;
                        changes.changed().await?;
                    }
                    shutdown @ qubit_executor::service::SubmissionError::Shutdown => {
                        return Err(shutdown.into());
                    }
                    error => return Err(error.into()),
                }
            }
        }
    };
    Ok(handle.await?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder().num_threads(2).task_capacity(8).build()?;
    let total = submit_after_capacity(&service, vec![8, 13, 21, 34].into_boxed_slice()).await?;
    assert_eq!(total, 76);
    service.shutdown();
    service.await_termination().await;
    Ok(())
}
```

The one-shot input remains owned by the same callable through saturation and is moved into the pool only after admission succeeds. Callers can use this retry loop to retain one-shot inputs without an additional shared ownership wrapper. This guide example uses Tokio to drive async waits and requires the `async-wait` feature described above.

`CapacityChanges::borrow` and `borrow_and_update` return Tokio watch guards that hold a read lock. Use them briefly, copy the generation, and drop the guard immediately. Do not hold a guard across `.await`, termination waits, or calls such as `shutdown`, `stop`, or `cancel` that can publish a notification. Prefer the copying methods `generation` and `generation_and_update` when retaining the value. The generation is a wrapping notification counter, not available capacity or a strict event count, and reading it does not reserve capacity.

`stats()` returns a consistent snapshot sampled under the service state lock.
Its `queued`, `running`, and `cancelling` counts all consume capacity.
`cancelling` includes cleanup still in progress after a task is removed from
the queue. A result becoming available and a cancellation cleanup finishing
can occur at slightly different times; observing or collecting a result does
not itself release capacity.

## Generic recoverable submission

The inherent method and the shared trait method have different rejection types.
A call through `RecoverableExecutorService` (as in this generic helper) returns
`qubit_executor::service::RejectedTask<C>`. A direct call on
`RayonExecutorService` returns this crate's `RejectedTask<C>`. Importing the
trait does not change method resolution: the concrete type's inherent method
still takes precedence. Both rejected values return ownership of the original
callable, and `into_parts()` provides its reason and callable. The existing
consuming submission methods continue to consume the task on rejection.

This synchronous example is the complete generic path. It returns 42 from an
accepted task, then confirms a later task is returned with `Shutdown`. The trait
adds no automatic wait or capacity retry; using a capacity notification still
requires the separate `async-wait` feature described above.

```rust
use std::io;
use qubit_executor::service::{ExecutorService, RecoverableExecutorService, RejectedTask};
use qubit_rayon_executor::RayonExecutorService;

fn submit_generic<S, C>(service: &S, task: C)
    -> Result<S::ResultHandle<usize, io::Error>, RejectedTask<C>>
where
    S: RecoverableExecutorService,
    C: FnMut() -> Result<usize, io::Error> + Send + 'static,
{
    service.try_submit_callable(task)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder().num_threads(1).build()?;
    let handle = submit_generic(&service, || Ok::<usize, io::Error>(42))?;
    service.shutdown();
    service.wait_termination();
    assert_eq!(handle.get()?, 42);
    let rejected = match submit_generic(&service, || Ok::<usize, io::Error>(7)) {
        Err(rejected) => rejected,
        Ok(_) => panic!("shutdown service accepted a task"),
    };
    let (reason, _task) = rejected.into_parts();
    assert_eq!(reason, qubit_executor::service::SubmissionError::Shutdown);
    Ok(())
}
```

## Advanced Usage

Tracked handles support status and best-effort cancellation before a Rayon
worker starts a task. A running task reports `AlreadyRunning`, and a task with a
terminal result reports `AlreadyFinished`. If `stop` or another cancellation
call has already claimed a queued task, a handle or cancellation endpoint can
still publish its `Cancelled` result while that owner is cleaning up the task.
The result may therefore be available before captured-value destructors finish;
the original owner keeps cleanup responsibility, and the task continues to
occupy capacity until cleanup completes.

If the result handle will be consumed into a future, first call
`RayonTaskHandle::cancellation_handle()` and retain the returned
`TaskCancellationHandle`. Its clones use the same Rayon queue cleanup and
capacity accounting path; dropping the endpoint does not cancel. Cancellation
is best-effort and cannot interrupt running work. Cleanup of captured values
for a cancelled queued task runs synchronously on the thread that first claimed
it. A concurrent cancellation call does not take over or wait for that cleanup.
Repeated calls after result publication report `AlreadyFinished`.

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
| `SubmissionError::Saturated` | Accepted unfinished work reached `task_capacity`; wait for a [capacity notification](#async-shutdown-and-capacity-waits) and retry the callable returned by `try_submit_callable`. Consuming submission methods require retained or reconstructed inputs. Alternatively reduce pressure, cancel queued tracked work, or choose a suitable capacity. |
| `SubmissionError::Shutdown` | The service no longer admits tasks; submit new work only to a running service. |
| `TaskExecutionError::Cancelled` | A queued tracked task was cancelled; resubmit only if the application still needs it. |

For submissions that return a result handle, task failures and panics are
observed through `get` or `.await`. Detached `submit` returns only admission
status and discards the final task result. Use a callable submission when the
caller needs to observe failure, or report failures from the detached task
itself.

Panic conversion applies to catchable unwinding panics. It cannot recover from
`panic=abort`, a second panic during unwinding, or a destructor that never
returns. Such cases may abort the process or prevent task cleanup and service
termination from completing.

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
