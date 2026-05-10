# Qubit Rayon Executor

[![CircleCI](https://circleci.com/gh/qubit-ltd/rs-rayon-executor.svg?style=shield)](https://circleci.com/gh/qubit-ltd/rs-rayon-executor)
[![Coverage Status](https://coveralls.io/repos/github/qubit-ltd/rs-rayon-executor/badge.svg?branch=main)](https://coveralls.io/github/qubit-ltd/rs-rayon-executor?branch=main)
[![Crates.io](https://img.shields.io/crates/v/qubit-rayon-executor.svg?color=blue)](https://crates.io/crates/qubit-rayon-executor)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![中文文档](https://img.shields.io/badge/文档-中文版-blue.svg)](README.zh_CN.md)

Rayon-backed CPU executor service for Rust.

## Overview

Qubit Rayon Executor adapts a dedicated Rayon thread pool to the Qubit
`ExecutorService` contract. It is intended for CPU-bound work where Rayon worker
scheduling is more appropriate than a general-purpose blocking queue.

The crate is separate from `qubit-thread-pool` and `qubit-tokio-executor` so
libraries can depend only on the execution model they need.

## Features

- `RayonExecutorService` for managed CPU-bound task execution.
- `RayonExecutorServiceBuilder` for configuring worker count, thread-name prefix, and stack size.
- `TaskHandle` for callable results and `RayonTaskHandle` for tracked status and cancellation.
- `RayonExecutorServiceBuildError` for zero thread count, zero stack size, and Rayon build failures.
- Shared `ExecutorService`, `RejectedExecution`, and `StopReport` re-exports for convenient imports.
- Lifecycle behavior aligned with other Qubit executor services.

## CPU-Bound Workloads

Rayon is optimized for CPU-bound parallel work. Use this crate when the workload
primarily consumes CPU and should run on a dedicated Rayon pool. Avoid using it
for long blocking IO operations, because blocking Rayon workers can reduce CPU
parallelism for unrelated tasks.

If your task is synchronous and may block on IO, prefer `qubit-thread-pool`. If
your task is an async future or must integrate with Tokio, prefer
`qubit-tokio-executor`.

## Shutdown and Cancellation

A successful `submit` means the service accepted a fire-and-forget runnable.
Use `submit_callable` for a result-only `TaskHandle`, or
`submit_tracked` / `submit_tracked_callable` when you need a `RayonTaskHandle`
with status and cancellation.

Queued tasks can be cancelled before Rayon starts running them. `shutdown` stops
accepting new tasks and allows accepted work to finish. `stop` stops
accepting new tasks and cancels work that has not started yet; already running
CPU work is not forcibly stopped.

## Quick Start

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

## Choosing an Executor

Use `RayonExecutorService` for CPU-heavy computations that should be isolated in
a Rayon pool. Use `qubit-thread-pool` for general blocking OS-thread work. Use
`qubit-tokio-executor` for Tokio blocking tasks and async IO futures.

For application-level wiring across blocking, CPU-bound, Tokio blocking, and
async IO domains, use `qubit-execution-services`.

## Testing

A minimal local run:

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

To mirror what continuous integration enforces, run the repository scripts from
the project root: `./align-ci.sh` brings local tooling and configuration in line
with CI, then `./ci-check.sh` runs the same checks the pipeline uses. For test
coverage, use `./coverage.sh` to generate or open reports.

## Contributing

Issues and pull requests are welcome.

- Open an issue for bug reports, design questions, or larger feature proposals when it helps align on direction.
- Keep pull requests scoped to one behavior change, fix, or documentation update when practical.
- Before submitting, run `./align-ci.sh` and then `./ci-check.sh` so your branch matches CI rules and passes the same checks as the pipeline.
- Add or update tests when you change runtime behavior, and update this README or public rustdoc when user-visible API behavior changes.
- If you change cancellation or shutdown behavior, include tests for queued and already-running tasks where practical.

By contributing, you agree to license your contributions under the [Apache License, Version 2.0](LICENSE), the same license as this project.

## License

Copyright (c) 2026. Haixing Hu.

This project is licensed under the [Apache License, Version 2.0](LICENSE). See the `LICENSE` file in the repository for the full text.

## Author

**Haixing Hu** — Qubit Co. Ltd.

| | |
| --- | --- |
| **Repository** | [github.com/qubit-ltd/rs-rayon-executor](https://github.com/qubit-ltd/rs-rayon-executor) |
| **Documentation** | [docs.rs/qubit-rayon-executor](https://docs.rs/qubit-rayon-executor) |
| **Crate** | [crates.io/crates/qubit-rayon-executor](https://crates.io/crates/qubit-rayon-executor) |
