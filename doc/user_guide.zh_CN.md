# Qubit Rayon Executor 用户手册

[English guide](user_guide.md) | 适用于 `qubit-rayon-executor` 0.9 与 Rust 1.94+

## 手册目标与读者

本手册面向需要将同步 CPU 密集型任务隔离到 Rayon 池中，同时继续使用 Qubit `ExecutorService` 生命周期和任务句柄 API 的 Rust 开发者。它不会把阻塞 IO 或 async future 变成 CPU 任务；这两类工作应分别交给线程池或 Tokio executor。

## 概念模型

`RayonExecutorServiceBuilder` 创建独立的 Rayon 池，`RayonExecutorService` 通过 `ExecutorService` 接受任务。每项已接纳任务都处于排队、运行或取消中之一，三种状态都会占用 `task_capacity`；默认容量为 1024。无需结果的 runnable 使用 `submit`；只关心结果时使用 `submit_callable`；需要状态或尽力取消尚未开始的任务时使用 tracked 提交方法。detached 提交只提供接纳状态；最终任务结果的去向见[错误与诊断](#错误与诊断)。内部唯一任务标识耗尽时，服务会停止接纳新任务并返回 `Shutdown`；已接纳的任务仍会继续完成。

```text
调用方 -> 已接纳任务的容量上限 -> Rayon 池 -> 任务句柄
                            \-> 容量用尽时返回 Saturated
```

克隆共享同一个线程池和生命周期。通过任一克隆调用 `shutdown` 或
`stop` 都会让所有克隆停止接收新任务。丢弃一个克隆不会隐式关闭、停止
或等待服务。若需要保证任务完成，请显式协调 `shutdown` 或 `stop`，并等待终止。

已接纳的任务不得同步等待其所属服务终止。终止条件包含所有已接纳任务完成，
因此该任务和终止等待会相互依赖。即使有空闲 worker，也不能解除这个循环；
先调用 `shutdown` 同样不能解除循环。请在任务外协调结果和服务终止。

## 贯穿场景：隔离一项 CPU 聚合计算

假设应用需要汇总一批数值，又不希望这项计算与其他执行域争用资源。完成标准是取得汇总值，然后正常结束接收新任务。

### 安装与最小配置

从 crates.io 添加以下两个 crate。提交方法所需 trait 来自 `qubit-executor`：

```toml
[dependencies]
qubit-executor = "0.9.0"
qubit-rayon-executor = "0.9"
```

从源码 checkout 验证下方示例可在仓库根目录运行
`cargo test --test documentation_examples_tests`。验证已发布的 crates.io
依赖组合时，请显式运行
`cargo test --test documentation_examples_tests test_registry_documentation_examples_compile_and_run -- --ignored --exact --nocapture`。
此 smoke test 需要访问 registry 网络，并使用独立 fixture、锁文件和构建目录。
输出记录精确解析的 crate 版本及 features；Cargo 会在 manifest 版本范围内选择最新的
兼容已发布版本，因此结论仅适用于这些版本。该 Registry 测试无法验证尚未发布的
候选版本；当前 checkout 使用源码测试和 package 检查验证。

### 核心工作流

先配置专用池，提交计算，取得结果后结束接收任务：

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

`submit_callable` 成功只表示服务接纳了任务，并不表示 callable 已经成功完成。请从 `TaskHandle` 取得结果，以观察任务自己的错误或 panic。`shutdown` 会让已接纳任务完成，`wait_termination` 则等待它们进入终态。

`ExecutorService::submit_callable` 即使返回 `Saturated` 或 `Shutdown` 也会消费 callable。对于只能使用一次的任务值，可使用本类型固有方法 `RayonExecutorService::try_submit_callable`：接纳失败时，它会在 `RejectedTask<C>` 中归还同一个 callable。调用 `into_parts()` 可取回原因和 callable；丢弃拒绝值时 callable 按常规析构。本 crate 的 `RejectedTask` 与 Tokio executor 中同名类型彼此独立。

## 异步关闭与容量等待

异步等待是可选功能。启用 crate 的 `async-wait` feature 后，可使用 `await_termination` 和 `capacity_changes`。等待 future 可由任何 Rust future executor 驱动，不要求 Tokio runtime。下面的示例使用 Tokio API，因此需要 Tokio runtime：

```toml
[dependencies]
qubit-executor = "0.9.0"
qubit-rayon-executor = { version = "0.9", features = ["async-wait"] }
tokio = { version = "1.54", features = ["macros", "rt-multi-thread"] }
```

在 async 代码中，调用 `shutdown` 或 `stop` 后优先使用 `await_termination`。同步的 `wait_termination` 会阻塞调用线程，不应在 Tokio runtime worker 上执行。这只表示终止等待本身不会阻塞线程：`stop` 以及成功取消队列任务的 `cancel` 会在调用线程同步析构捕获值，耗时取决于用户清理逻辑。即使取消结果已经可见，这些任务在清理完成前仍计入 `cancelling`。不要让清理代码同步等待依赖该清理完成的终止条件。

如果 `stop` 的清理可能耗时，请将它放入 `spawn_blocking`，再异步等待终止：

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

此示例需要启用 `async-wait` feature 并运行 Tokio runtime（上面的依赖配置已启用 Tokio 的 `rt-multi-thread`）。`?` 会传播 blocking task 在启动前被取消或发生 panic 时的 `JoinError`。丢弃 join future 不会强制终止已经开始运行的 blocking closure。`stop` 仍不会中断正在运行的 CPU 任务；如果任务不返回，终止等待也可能一直持续。

处理背压时，请先订阅，再提交。收到 `Saturated` 后等待变化，并重试 `RejectedTask` 归还的 callable。通知只是提示：其他生产者可能先占用容量，而且 API 不保证 FIFO、公平性或有限等待。应将 `Shutdown` 和通知通道关闭视为终止结果：

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

饱和时，原任务值仍由同一个 callable 持有；只有接纳成功才会将其移入执行池。调用方可用此重试循环保留一次性输入，无需额外的共享所有权包装。此手册示例用 Tokio 驱动异步等待，因此需要启用上方的 `async-wait` feature。

`CapacityChanges::borrow` 和 `borrow_and_update` 返回 Tokio watch guard，并持有读锁。应短时读取、复制 generation 后立即释放 guard。不要跨 `.await`、终止等待，或调用 `shutdown`、`stop`、`cancel` 等可能发布通知的操作持有 guard。需要保留数值时优先使用复制读取方法 `generation` 和 `generation_and_update`。generation 是可回绕的通知代数，不表示可用容量或严格事件数，读取也不会预留容量。

`stats()` 返回在服务状态锁内采样的一致快照。`queued`、`running` 和 `cancelling` 都会占用容量。任务从队列移除后，`cancelling` 仍会统计尚未完成的清理。结果可用与取消清理完成可能存在短暂时间差；观察或收取结果本身不会释放容量。

## 泛型恢复提交

固有方法和共享 trait 方法的拒绝值类型不同。通过
`RecoverableExecutorService` 调用（例如下面的泛型辅助函数）会返回
`qubit_executor::service::RejectedTask<C>`；直接在 `RayonExecutorService`
上调用则返回本 crate 自己的 `RejectedTask<C>`。即使导入了 trait，具体类型的
固有方法仍优先匹配，因此不会改变原有调用的返回类型。两种拒绝值都会归还原
callable；调用 `into_parts()` 可分别取出拒绝原因和任务。原有消费型提交方法在
拒绝时仍会消费任务。

下面是完整的同步泛型示例：已接纳的任务返回 42；服务关闭后再次提交会得到
`Shutdown` 和原任务。这个 trait 不会自动等待容量释放，也不会自动重试；使用容量
通知仍须启用上文所述的独立 `async-wait` feature。

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

## 进阶用法

tracked 句柄支持查询状态，并可在 Rayon worker 启动任务前尽力取消。任务正在运行时返回 `AlreadyRunning`；已有终态结果时返回 `AlreadyFinished`。如果 `stop` 或另一项取消操作已经认领了排队任务，句柄或取消端点仍能发布 `Cancelled` 结果，同时由原认领方继续清理任务。此时结果可能先于捕获值析构完成而可见；清理责任不会转移，在清理结束前该任务仍占用容量。

如果要将结果句柄转换为 future，请先调用
`RayonTaskHandle::cancellation_handle()` 并保留返回的
`TaskCancellationHandle`。克隆句柄会复用 Rayon 队列清理和容量记账路径；丢弃端点
不会取消任务。取消仍是尽力而为，不能中断已运行的任务。最先认领排队任务的线程会同步清理捕获值；并发的取消调用既不会接管清理，也不会等待清理结束。结果发布后再次取消会返回 `AlreadyFinished`。

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
        started_tx.send(()).expect("记录任务已启动");
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

关闭时若要放弃队列任务，请调用 `stop`。它的 `StopReport` 会记录排队、运行和已取消的任务数，但不能强制中断已经运行的 Rayon 任务。

## 错误与诊断

| 现象 | 含义与处理 |
| --- | --- |
| `ZeroThreadCount`、`ZeroStackSize` 或 `ZeroTaskCapacity` | builder 的值为零；请配置非零值。 |
| `SubmissionError::Saturated` | 未完成的已接纳任务达到 `task_capacity`；等待[容量通知](#异步关闭与容量等待)后，重试 `try_submit_callable` 归还的 callable。消费型提交方法需要预先保留或重建输入。也可降低提交压力、取消排队中的 tracked 任务，或选择合适容量。 |
| `SubmissionError::Shutdown` | 服务已不再接纳任务；新任务只能提交给仍在运行的服务。 |
| `TaskExecutionError::Cancelled` | 队列中的 tracked 任务已取消；仅在业务仍需要时重新提交。 |

对于返回结果句柄的提交，任务失败或 panic 通过 `get` 或 `.await` 观察。detached `submit` 只返回接纳状态，并丢弃任务最终结果。调用方需要观察失败时，请改用 callable 提交，或由 detached 任务自行报告错误。

panic 转换仅适用于可捕获的展开式 panic。它无法恢复 `panic=abort`、展开期间再次发生的 panic，也无法让永不返回的析构函数完成。这些情况可能导致进程中止，或使任务清理和服务终止无法完成。

## 排障

容量统计的是排队、运行和取消中的已接纳任务，而不仅是可见队列项。收取结果不会释放容量；任务完成或取消清理完成后才释放。`wait_termination` 一直没有返回时，请调用 `shutdown` 或 `stop`，并检查是否有已接纳任务被阻塞。不要让一项任务在同一个已饱和的池中同步等待另一项任务；请使用 Rayon `join`/`scope` 或异步协调。长时间 CPU 任务若需要中断，应由任务自身实现协作式取消；本服务不会强制停止它。

## 限制与最佳实践

本服务仅适合同步 CPU 密集型工作。耗时的阻塞 IO 会降低池的可用并行度，async future 应交给 async executor。任务捕获的数据应保持有界；`task_capacity` 应视为背压上限而不是内存缓冲区。接收任务结束后，始终调用 `shutdown` 或 `stop`。

## 延伸阅读

- [README](../README.zh_CN.md)
- [English user guide](user_guide.md)
- [API 文档](https://docs.rs/qubit-rayon-executor)
