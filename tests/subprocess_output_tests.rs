// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
mod common;

use std::io::Write;
use std::process::Command;
use std::time::Duration;

const CHILD_MODE: &str = "RXE_SUBPROCESS_OUTPUT_CHILD";
const LARGE_SIZE: usize = 4 * 1024 * 1024;
const OUTER_DEADLINE: Duration = Duration::from_secs(20);
const CHILD_TEST_PREAMBLE: &[u8] = b"\nrunning 1 test\n";

#[test]
fn test_child_output() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    match mode.as_str() {
        "large" => {
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            let mut stdout = stdout.lock();
            let mut stderr = stderr.lock();
            stdout.write_all(&vec![b'o'; LARGE_SIZE]).expect("write stdout payload");
            stdout.write_all(b"stdout-end-marker").expect("write stdout marker");
            stderr.write_all(&vec![b'e'; LARGE_SIZE]).expect("write stderr payload");
            stderr.write_all(b"stderr-end-marker").expect("write stderr marker");
            stdout.flush().expect("flush stdout");
            stderr.flush().expect("flush stderr");
            drop(stdout);
            drop(stderr);
            std::process::exit(0);
        }
        "nonzero" => {
            eprintln!("child-exit-diagnostic");
            std::process::exit(7);
        }
        "timeout" => {
            eprintln!("child-timeout-diagnostic");
            std::thread::sleep(Duration::from_secs(30));
        }
        _ => panic!("unknown child mode: {mode}"),
    }
}

fn command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable path"));
    command
        .args(["--quiet", "--exact", "test_child_output", "--nocapture"])
        .env(CHILD_MODE, mode);
    command
}

#[test]
fn test_bounded_output_captures_failures_timeouts_and_cleans_logs() {
    let mut large = command("large");
    let large = common::subprocess::run_bounded_output(&mut large, OUTER_DEADLINE).expect("large-output child runs");
    assert!(!large.timed_out, "large-output child should finish");
    assert!(large.output.status.success(), "status: {}", large.output.status);
    assert!(
        large.output.stdout.starts_with(CHILD_TEST_PREAMBLE),
        "child harness preamble should be isolated"
    );
    assert_eq!(
        large.output.stdout.len(),
        CHILD_TEST_PREAMBLE.len() + LARGE_SIZE + b"stdout-end-marker".len()
    );
    let child_stdout = &large.output.stdout[CHILD_TEST_PREAMBLE.len()..];
    assert_eq!(child_stdout.len(), LARGE_SIZE + b"stdout-end-marker".len());
    assert_eq!(large.output.stderr.len(), LARGE_SIZE + b"stderr-end-marker".len());
    assert!(child_stdout[..LARGE_SIZE].iter().all(|byte| *byte == b'o'));
    assert_eq!(&child_stdout[LARGE_SIZE..], b"stdout-end-marker");
    assert!(large.output.stderr[..LARGE_SIZE].iter().all(|byte| *byte == b'e'));
    assert_eq!(&large.output.stderr[LARGE_SIZE..], b"stderr-end-marker");
    assert!(
        large.log_paths.iter().all(|path| !path.exists()),
        "large child logs should be removed"
    );

    let mut nonzero = command("nonzero");
    let nonzero = common::subprocess::run_bounded_output(&mut nonzero, OUTER_DEADLINE).expect("nonzero child runs");
    assert!(
        nonzero.log_paths.iter().all(|path| !path.exists()),
        "nonzero child logs should be removed"
    );
    assert!(!nonzero.timed_out);
    assert_eq!(nonzero.output.status.code(), Some(7));
    assert!(
        nonzero
            .output
            .stderr
            .windows(b"child-exit-diagnostic".len())
            .any(|window| window == b"child-exit-diagnostic")
    );

    let mut timeout = command("timeout");
    let timeout =
        common::subprocess::run_bounded_output(&mut timeout, Duration::from_millis(100)).expect("timeout child starts");
    assert!(
        timeout.log_paths.iter().all(|path| !path.exists()),
        "timed-out child logs should be removed"
    );
    assert!(timeout.timed_out);
    assert!(!timeout.output.status.success());
    assert!(
        timeout
            .output
            .stderr
            .windows(b"child-timeout-diagnostic".len())
            .any(|window| window == b"child-timeout-diagnostic")
    );

    let mut invalid = Command::new(std::env::temp_dir().join(format!("missing-rxe-executable-{}", std::process::id())));
    assert!(common::subprocess::run_bounded_output(&mut invalid, OUTER_DEADLINE).is_err());

    let mut first = command("nonzero");
    let first = common::subprocess::run_bounded_output(&mut first, OUTER_DEADLINE).expect("first repeated child runs");
    let mut second = command("large");
    let second =
        common::subprocess::run_bounded_output(&mut second, OUTER_DEADLINE).expect("second repeated child runs");
    assert!(
        first.log_paths.iter().all(|path| !path.exists()),
        "first repeated child logs should be removed"
    );
    assert!(
        second.log_paths.iter().all(|path| !path.exists()),
        "second repeated child logs should be removed"
    );
    assert!(first.output.stderr.ends_with(b"child-exit-diagnostic\n"));
    assert!(second.output.stderr.ends_with(b"stderr-end-marker"));
}
