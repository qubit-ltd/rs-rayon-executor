# Qubit Rayon Executor

[![Rust CI](https://github.com/qubit-ltd/rs-rayon-executor/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-rayon-executor/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https://qubit-ltd.github.io/rs-rayon-executor/coverage-badge.json)](https://qubit-ltd.github.io/rs-rayon-executor/coverage/)
[![Crates.io](https://img.shields.io/crates/v/qubit-rayon-executor.svg?color=blue)](https://crates.io/crates/qubit-rayon-executor)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![English Documentation](https://img.shields.io/badge/docs-English-blue.svg)](README.md)

面向 Rust 的 Rayon-backed CPU executor service。

## 概览

Qubit Rayon Executor 将专用 Rayon thread pool 适配到 Qubit `ExecutorService` 契约。它面向 CPU 密集型工作，适用于 Rayon worker 调度比通用 blocking 队列更合适的场景。

本 crate 与 `qubit-thread-pool`、`qubit-tokio-executor` 分离，便于库作者只依赖自己需要的执行模型。

## 功能

- 提供 `RayonExecutorService`，用于托管 CPU 密集型任务执行。
- 提供 `RayonExecutorServiceBuilder`，用于配置 worker 数量、线程名前缀与栈大小。
- 提供 `TaskHandle` 用于 callable 结果，提供 `RayonTaskHandle` 用于 tracked 状态与取消。
- 提供 `RayonExecutorServiceBuildError`，表示线程数为零、栈大小为零或 Rayon 构建失败。
- 再导出共享的 `ExecutorService`、`SubmissionError` 与 `StopReport`，便于使用方导入。
- 生命周期行为与其它 Qubit executor service 对齐。

## CPU 密集型负载

Rayon 针对 CPU 密集型并行工作优化。当负载主要消耗 CPU，并且应该运行在专用 Rayon pool 中时，使用本 crate。避免把它用于长时间 blocking IO 操作，因为阻塞 Rayon worker 会降低无关任务的 CPU 并行度。

如果任务是同步且可能阻塞 IO，优先使用 `qubit-thread-pool`。如果任务是 async future 或必须与 Tokio 集成，优先使用 `qubit-tokio-executor`。

## 关闭与取消

`submit` 成功只表示服务接受了一个 fire-and-forget runnable。需要结果时使用 `submit_callable` 获取只包含结果的 `TaskHandle`；需要状态和取消时，使用 `submit_tracked` 或 `submit_tracked_callable` 获取 `RayonTaskHandle`。

队列中的任务在 Rayon 开始运行前可以被取消。`shutdown` 停止接受新任务，并允许已接受工作完成。`stop` 停止接受新任务，并取消尚未开始的工作；已经运行的 CPU 工作不会被强制停止。被取消的 callable 与 tracked handle 会报告 `TaskExecutionError::Cancelled`。

## 快速开始

```rust
use std::io;

use qubit_rayon_executor::{ExecutorService, RayonExecutorService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder()
        .num_threads(4)
        .thread_name_prefix("cpu-worker")
        .build()?;

    let handle = service.submit_callable(|| Ok::<usize, io::Error>((1..=10).sum()))?;
    assert_eq!(handle.get()?, 55);
    service.shutdown();
    Ok(())
}
```

## 如何选择 Executor

CPU 密集型计算需要隔离在 Rayon pool 中时，使用 `RayonExecutorService`。通用 blocking OS 线程工作使用 `qubit-thread-pool`。Tokio blocking 任务和 async IO future 使用 `qubit-tokio-executor`。

应用层需要统一装配 blocking、CPU、Tokio blocking 与 async IO 域时，使用 `qubit-execution-services`。

## 测试

快速在本地跑一遍：

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

若要与持续集成（CI）保持一致，请在仓库根目录依次执行：`./align-ci.sh` 将本地工具链与配置对齐到 CI 规则，再执行 `./ci-check.sh` 复现流水线中的检查。需要查看或生成测试覆盖率时，使用 `./coverage.sh`。

## 参与贡献

欢迎通过 Issue 与 Pull Request 参与本仓库。建议：

- 报告缺陷、讨论设计或较大能力扩展时，可先开 Issue 对齐方向再投入实现。
- 单次 PR 尽量聚焦单一主题，便于代码审查与合并历史。
- 提交 PR 前请先运行 `./align-ci.sh`，再运行 `./ci-check.sh`，确保本地与 CI 使用同一套规则且能通过流水线等价检查。
- 若修改运行期行为，请补充或更新相应测试；若影响对外 API 或用户可见行为，请同步更新本文档或相关 rustdoc。
- 如果修改取消或关闭行为，请尽量覆盖队列任务和已运行任务的测试。

向本仓库贡献内容即表示您同意以 [Apache License, Version 2.0](LICENSE)（与本项目相同）授权您的贡献。

## 许可证与版权

Copyright (c) 2026. Haixing Hu.

本软件依据 [Apache License, Version 2.0](LICENSE) 授权；完整许可文本见仓库根目录的 `LICENSE` 文件。

## 作者与维护

**Haixing Hu** — Qubit Co. Ltd.

| | |
| --- | --- |
| **源码仓库** | [github.com/qubit-ltd/rs-rayon-executor](https://github.com/qubit-ltd/rs-rayon-executor) |
| **API 文档** | [docs.rs/qubit-rayon-executor](https://docs.rs/qubit-rayon-executor) |
| **Crate 发布** | [crates.io/crates/qubit-rayon-executor](https://crates.io/crates/qubit-rayon-executor) |
