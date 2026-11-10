# Qubit Rayon Executor

[![Rust CI](https://github.com/qubit-ltd/rs-rayon-executor/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-rayon-executor/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https://qubit-ltd.github.io/rs-rayon-executor/coverage-badge.json)](https://qubit-ltd.github.io/rs-rayon-executor/coverage/)
[![Crates.io](https://img.shields.io/crates/v/qubit-rayon-executor.svg?color=blue)](https://crates.io/crates/qubit-rayon-executor)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![English Document](https://img.shields.io/badge/Document-English-blue.svg)](README.md)

Qubit Rayon Executor 为需要处理 CPU 密集型同步任务的 Rust 库提供独立且有容量上限的 Rayon 线程池。调用方可以沿用 Qubit executor 契约提交任务，避免通用阻塞队列与应用的 CPU 计算争用资源。

## 安装

从 crates.io 添加以下两个 crate。提交方法所需的 trait 来自 `qubit-executor`：

```toml
[dependencies]
qubit-executor = "0.9.1"
qubit-rayon-executor = "0.9"
```

本 crate 需要 Rust 1.94 或更高版本。

## 快速开始

例如，可将一项 CPU 聚合计算隔离到专用 Rayon 池中：

```rust
use std::io;

use qubit_executor::service::ExecutorService;
use qubit_rayon_executor::RayonExecutorService;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = RayonExecutorService::builder()
        .num_threads(4)
        .task_capacity(1024)
        .thread_name_prefix("cpu-worker")
        .build()?;
    let handle = service.submit_callable(|| Ok::<usize, io::Error>((1..=10).sum()))?;
    assert_eq!(handle.get()?, 55);
    service.shutdown();
    service.wait_termination();
    Ok(())
}
```

## 为什么需要它

CPU 密集型任务适合由 Rayon 调度；耗时的阻塞 IO 会占用本应服务于其他计算的 worker。本 crate 将 CPU 工作放入独立的 Rayon 池，并限制已接纳但未完成的任务数量，让过载变得可观察而非无限堆积。

同步任务若可能阻塞 IO，请使用 `qubit-thread-pool`；Tokio 的阻塞任务或 async IO future，请使用 `qubit-tokio-executor`。

## 核心能力与边界

- `RayonExecutorService` 及其 builder：创建可配置的 Rayon 池。
- 支持无需结果、只需结果和 tracked 三类提交；tracked 句柄可查询状态，并尽力取消尚未开始的任务。
- `try_submit_callable` 在拒绝接纳时归还原 callable；现有提交方法仍会在拒绝时消费任务。
  `RayonExecutorService` 也实现了共享的 `RecoverableExecutorService` trait。两种拒绝类型的区别及重试方法见[用户手册](doc/user_guide.zh_CN.md)。
- 已接纳任务默认容量为 1024，达到上限时返回 `SubmissionError::Saturated`。
- `stats()` 提供一致状态快照；启用 `async-wait` feature 后还可异步等待终止并订阅容量变化。
- `shutdown` 会完成已接纳的任务；`stop` 会取消排队任务，但二者都无法强制停止已经运行的 CPU 任务。
- `RayonExecutorServiceBuildError` 表示线程数、栈大小或容量无效，以及 Rayon 池构建失败。

它不是 async runtime，也不能替代 Rayon 的 `join` 和 `scope`。不要在同一个已饱和的池中同步等待另一项任务。

## 延伸阅读

[English user guide](doc/user_guide.md) 与[中文用户手册](doc/user_guide.zh_CN.md)说明生命周期和取消细节；完整公共 API 见 [API 文档](https://docs.rs/qubit-rayon-executor)，也可阅读 [English README](README.md)。

## 测试

从源码参与开发时，请在本仓库旁保留兼容的 `rs-executor` checkout。
当前路径依赖要求 `qubit-executor` `^0.9.1`，即 0.9.1 或后续兼容的
0.9 版本；0.9.0 不满足要求。文档示例测试也会将该依赖 patch 到相邻源码。

    parent/
    ├── rs-executor/
    └── rs-rayon-executor/

在本仓库中运行 `./.infra/bin/prepare-local-path-dependencies.sh`，
创建缺少的本地路径依赖 checkout，再执行下列测试命令。该脚本会保留已有
checkout，不会 fetch、重置或替换它。测试前，开发者须确认已有的
`rs-executor` checkout 使用兼容的 `qubit-executor` 版本和 API。创建缺失的
checkout 时可能访问网络。这些源码开发前置条件不适用于 crates.io 安装。

```bash
# 使用默认 feature 集运行测试
cargo test

# 使用项目声明的全部 feature 运行测试
cargo test --all-features

# 运行项目 CI 检查
./.infra/bin/ci-check.sh

# 检查代码覆盖率
./.infra/bin/coverage.sh
```

## 许可证

Copyright (c) 2025 - 2026. Haixing Hu. All rights reserved.

本项目基于 Apache License 2.0 授权。完整许可证文本请参阅
[LICENSE](LICENSE)。

## 贡献

欢迎贡献。请遵循 Rust API 指南，及时更新公共 API 文档与测试，并在提交
Pull Request 前运行 `./.infra/bin/align-ci.sh`格式化代码，运行`./.infra/bin/ci-check.sh`对齐CI要求。
源码开发依赖准备说明请参阅[测试](#测试)。

## 作者

**Haixing Hu** - *Qubit Co. Ltd.*

仓库地址：[https://github.com/qubit-ltd/rs-rayon-executor](https://github.com/qubit-ltd/rs-rayon-executor)
