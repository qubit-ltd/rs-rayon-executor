# Qubit Rayon Executor

[![Rust CI](https://github.com/qubit-ltd/rs-rayon-executor/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-rayon-executor/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https://qubit-ltd.github.io/rs-rayon-executor/coverage-badge.json)](https://qubit-ltd.github.io/rs-rayon-executor/coverage/)
[![Crates.io](https://img.shields.io/crates/v/qubit-rayon-executor.svg?color=blue)](https://crates.io/crates/qubit-rayon-executor)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![中文文档](https://img.shields.io/badge/文档-中文版-blue.svg)](README.zh_CN.md)

Qubit Rayon Executor gives Rust libraries a bounded, dedicated Rayon pool for
CPU-bound synchronous work. It lets callers submit work through the Qubit
executor contract without making a general blocking queue compete for CPU.

## Installation

Add both crates from crates.io. Submission methods require the executor trait:

```toml
[dependencies]
qubit-executor = "0.8"
qubit-rayon-executor = "0.8"
```

Rust 1.94 or later is required.

## Quick Start

For example, isolate a CPU aggregation in its own Rayon pool:

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

## Why This Project Exists

CPU-heavy work benefits from Rayon scheduling, while long blocking IO can occupy
workers needed by other computation. This crate isolates CPU work and bounds
accepted unfinished tasks, making overload observable rather than unbounded.

Use `qubit-thread-pool` for synchronous work that may block on IO, and
`qubit-tokio-executor` for Tokio blocking tasks or async IO futures.

## What It Provides

- `RayonExecutorService` and its builder for a configurable Rayon pool.
- Detached, result-only, and tracked submissions; tracked handles expose status
  and best-effort cancellation before a worker starts the task.
- A default accepted-task capacity of 1024 and `SubmissionError::Saturated` at
  capacity.
- `shutdown` to finish accepted work, and `stop` to cancel queued work; neither
  forcibly stops CPU work that has already started.
- `RayonExecutorServiceBuildError` for invalid thread, stack-size, and capacity
  settings, and Rayon pool-construction failures.

It is not an async runtime or a replacement for Rayon `join` and `scope`. Do
not synchronously wait for another task from the same saturated pool.

## Learn More

Read the [English user guide](doc/user_guide.md) or
[中文用户手册](doc/user_guide.zh_CN.md) for lifecycle and cancellation details;
see the [API documentation](https://docs.rs/qubit-rayon-executor) or the
[中文 README](README.zh_CN.md) for more.

## Testing

```bash
# Run tests with the default feature set
cargo test

# Run tests with all declared features
cargo test --all-features

# Project CI checks
./ci-check.sh

# Check code coverage
./coverage.sh
```

## License

Copyright (c) 2025 - 2026. Haixing Hu. All rights reserved.

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the
full license text.

## Contributing

Contributions are welcome. Please follow the Rust API guidelines, keep public
API documentation and tests current, and run `./align-ci.sh` to format code and
`./ci-check.sh` to satisfy CI requirements before submitting a pull request.

## Author

**Haixing Hu** - *Qubit Co. Ltd.*

Repository: [https://github.com/qubit-ltd/rs-rayon-executor](https://github.com/qubit-ltd/rs-rayon-executor)
