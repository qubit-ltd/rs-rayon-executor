# Qubit Rayon Executor 用户手册

[English guide](user_guide.md) | 适用于 `qubit-rayon-executor` 0.9 与 Rust 1.94+

## 手册目标与读者

本手册面向需要将同步 CPU 密集型任务隔离到 Rayon 池中，同时继续使用 Qubit `ExecutorService` 生命周期和任务句柄 API 的 Rust 开发者。它不会把阻塞 IO 或 async future 变成 CPU 任务；这两类工作应分别交给线程池或 Tokio executor。

## 概念模型

`RayonExecutorServiceBuilder` 创建独立的 Rayon 池，`RayonExecutorService` 通过 `ExecutorService` 接受任务。每项已接纳任务都处于排队、运行或取消中之一，三种状态都会占用 `task_capacity`；默认容量为 1024。无需结果的 runnable 使用 `submit`；只关心结果时使用 `submit_callable`；需要状态或尽力取消尚未开始的任务时使用 tracked 提交方法。

```text
调用方 -> 已接纳任务的容量上限 -> Rayon 池 -> 任务句柄
                            \-> 容量用尽时返回 Saturated
```

## 贯穿场景：隔离一项 CPU 聚合计算

假设应用需要汇总一批数值，又不希望这项计算与其他执行域争用资源。完成标准是取得汇总值，然后正常结束接收新任务。

### 安装与最小配置

从 crates.io 添加以下两个 crate。提交方法所需 trait 来自 `qubit-executor`：

```toml
[dependencies]
qubit-executor = "0.9"
qubit-rayon-executor = "0.9"
```

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

提交任务会消费 callable，即使提交返回 `Saturated` 或 `Shutdown` 也是如此。请在闭包外保留重试所需的输入，并为每次尝试重新构造 callable。对于只能使用一次的任务值，请在提交成功前保留共享所有权，或使用能够保留任务的重试 API。

## 异步关闭与容量等待

异步等待是可选功能。启用 crate 的 `async-wait` feature 后，可使用 `await_termination` 和 `capacity_changes`；运行下面的异步示例需要 Tokio runtime：

```toml
[dependencies]
qubit-executor = "0.9"
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

处理背压时，请先订阅，再提交。收到 `Saturated` 后等待变化并使用新 callable 重试。通知只是提示：其他生产者可能先占用容量，而且 API 不保证 FIFO、公平性或有限等待。应将 `Shutdown` 和通知通道关闭视为终止结果：

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
                    return Err("服务通知通道已关闭".into());
                }
            }
            Err(SubmissionError::Shutdown) => return Err("服务已关闭".into()),
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

`stats()` 返回在服务状态锁内采样的一致快照。`queued`、`running` 和 `cancelling` 都会占用容量。任务从队列移除后，`cancelling` 仍会统计尚未完成的清理。结果可用与取消清理完成可能存在短暂时间差；观察或收取结果本身不会释放容量。

## 进阶用法

tracked 句柄支持查询状态和取消。任务仍在队列中时取消会成功；已运行的任务报告 `AlreadyRunning`，已完成的任务报告 `AlreadyFinished`。

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
| `SubmissionError::Saturated` | 未完成的已接纳任务达到 `task_capacity`；等待容量通知，并使用新构造的 callable 重试；也可降低提交压力、取消队列中的 tracked 任务，或选择合适容量。 |
| `SubmissionError::Shutdown` | 服务已不再接纳任务；新任务只能提交给仍在运行的服务。 |
| `TaskExecutionError::Cancelled` | 队列中的 tracked 任务已取消；仅在业务仍需要时重新提交。 |

任务自身失败或 panic 会通过 `get` 或 `.await` 返回，不能把提交成功当作计算成功。

## 排障

容量统计的是排队、运行和取消中的已接纳任务，而不仅是可见队列项。收取结果不会释放容量；任务完成或取消清理完成后才释放。`wait_termination` 一直没有返回时，请调用 `shutdown` 或 `stop`，并检查是否有已接纳任务被阻塞。不要让一项任务在同一个已饱和的池中同步等待另一项任务；请使用 Rayon `join`/`scope` 或异步协调。长时间 CPU 任务若需要中断，应由任务自身实现协作式取消；本服务不会强制停止它。

## 限制与最佳实践

本服务仅适合同步 CPU 密集型工作。耗时的阻塞 IO 会降低池的可用并行度，async future 应交给 async executor。任务捕获的数据应保持有界；`task_capacity` 应视为背压上限而不是内存缓冲区。接收任务结束后，始终调用 `shutdown` 或 `stop`。

## 延伸阅读

- [README](../README.zh_CN.md)
- [English user guide](user_guide.md)
- [API 文档](https://docs.rs/qubit-rayon-executor)
