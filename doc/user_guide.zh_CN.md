# Qubit Rayon Executor 用户手册

[English guide](user_guide.md) | 适用于 `qubit-rayon-executor` 0.7.0 与 Rust 1.94+

## 手册目标与读者

本手册面向需要将同步 CPU 密集型任务隔离到 Rayon 池中，同时继续使用 Qubit `ExecutorService` 生命周期和任务句柄 API 的 Rust 开发者。它不会把阻塞 IO 或 async future 变成 CPU 任务；这两类工作应分别交给线程池或 Tokio executor。

## 概念模型

`RayonExecutorServiceBuilder` 创建独立的 Rayon 池，`RayonExecutorService` 通过 `ExecutorService` 接受任务。每项已接纳任务都处于排队、运行或取消中之一，三种状态都会占用 `task_capacity`；默认容量为 1024。无需结果的 runnable 使用 `submit`；只关心结果时使用 `submit_callable`；需要状态或尽力取消尚未开始的任务时使用 tracked 提交方法。

```
调用方 -> 已接纳任务的容量上限 -> Rayon 池 -> 任务句柄
                            \-> 容量用尽时返回 Saturated
```

## 贯穿场景：隔离一项 CPU 聚合计算

假设应用需要汇总一批数值，又不希望这项计算与其他执行域争用资源。完成标准是取得汇总值，然后正常结束接收新任务。

### 安装与最小配置

该包配置了 `publish = false`，请从 Qubit 源码树或工作区中引用。提交方法所需 trait 来自 `qubit-executor`：

```toml
[dependencies]
qubit-executor = "0.8"
qubit-rayon-executor = { path = "../rs-rayon-executor" }
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
    let handle = service.submit_callable(move || Ok::<usize, io::Error>(values.into_iter().sum()))?;
    let total = handle.get()?;
    service.shutdown();
    service.wait_termination();
    Ok(total)
}

assert_eq!(aggregate()?, 76);
```

`submit_callable` 成功只表示服务接纳了任务，并不表示 callable 已经成功完成。请从 `TaskHandle` 取得结果，以观察任务自己的错误或 panic。`shutdown` 会让已接纳任务完成，`wait_termination` 则等待它们进入终态。

## 进阶用法

tracked 句柄支持查询状态和取消。任务仍在队列中时取消会成功；已运行的任务报告 `AlreadyRunning`，已完成的任务报告 `AlreadyFinished`。

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
```

关闭时若要放弃队列任务，请调用 `stop`。它的 `StopReport` 会记录排队、运行和已取消的任务数，但不能强制中断已经运行的 Rayon 任务。

## 错误与诊断

| 现象 | 含义与处理 |
| --- | --- |
| `ZeroThreadCount`、`ZeroStackSize` 或 `ZeroTaskCapacity` | builder 的值为零；请配置非零值。 |
| `SubmissionError::Saturated` | 未完成的已接纳任务达到 `task_capacity`；降低提交压力、及时取得结果、取消队列中的 tracked 任务，或选择合适容量。 |
| `SubmissionError::Shutdown` | 服务已不再接纳任务；新任务只能提交给仍在运行的服务。 |
| `TaskExecutionError::Cancelled` | 队列中的 tracked 任务已取消；仅在业务仍需要时重新提交。 |

任务自身失败或 panic 会通过 `get` 或 `.await` 返回，不能把提交成功当作计算成功。

## 排障

容量统计的是排队、运行和取消中的已接纳任务，而不仅是可见队列项。`wait_termination` 一直没有返回时，请调用 `shutdown` 或 `stop`，并检查是否有已接纳任务被阻塞。不要让一项任务在同一个已饱和的池中同步等待另一项任务；请使用 Rayon `join`/`scope` 或异步协调。长时间 CPU 任务若需要中断，应由任务自身实现协作式取消；本服务不会强制停止它。

## 限制与最佳实践

本服务仅适合同步 CPU 密集型工作。耗时的阻塞 IO 会降低池的可用并行度，async future 应交给 async executor。任务捕获的数据应保持有界；`task_capacity` 应视为背压上限而不是内存缓冲区。接收任务结束后，始终调用 `shutdown` 或 `stop`。

## 延伸阅读

- [README](../README.zh_CN.md)
- [English user guide](user_guide.md)
- [API 文档](https://docs.rs/qubit-rayon-executor)
