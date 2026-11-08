// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Private implementations for the executor's queued work.

mod admission;
mod callable_job;
mod cancel_disposition;
#[cfg(feature = "async-wait")]
mod isolated_wakers;
#[cfg(feature = "async-wait")]
mod panic_isolating_wake;
mod panic_payload;
mod rayon_executor_service_inner;
mod runnable_job;
#[cfg(feature = "async-wait")]
mod waker_panic_isolation;

pub(crate) use admission::Admission;
pub(crate) use callable_job::CallableJob;
pub(crate) use cancel_disposition::CancelDisposition;
#[cfg(feature = "async-wait")]
pub(crate) use isolated_wakers::IsolatedWakers;
#[cfg(feature = "async-wait")]
pub(crate) use panic_isolating_wake::PanicIsolatingWake;
pub(crate) use panic_payload::discard_panic_payload;
pub(crate) use rayon_executor_service_inner::RayonExecutorServiceInner;
pub(crate) use runnable_job::RunnableJob;
#[cfg(feature = "async-wait")]
pub(crate) use waker_panic_isolation::WakerPanicIsolation;
#[cfg(feature = "async-wait")]
pub(crate) use waker_panic_isolation::poll_with_waker_panic_isolation;
