// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;
mod documentation_examples;

#[test]
fn test_documentation_examples_compile_and_run() {
    documentation_examples::check_all_documents();
}

#[test]
#[ignore = "requires crates.io access; run explicitly for release acceptance"]
fn test_registry_documentation_examples_compile_and_run() {
    documentation_examples::check_all_registry_documents();
}
