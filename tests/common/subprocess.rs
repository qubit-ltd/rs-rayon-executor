// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
#![allow(dead_code)]

use std::fs;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

static NEXT_LOG_ID: AtomicU64 = AtomicU64::new(0);

/// Captured output and timeout status from a bounded child process.
pub(crate) struct BoundedOutput {
    /// Child process status and captured standard streams.
    pub(crate) output: Output,
    /// Temporary files that captured standard output and standard error.
    pub(crate) log_paths: [PathBuf; 2],
    /// Whether the direct child exceeded its deadline.
    pub(crate) timed_out: bool,
}

struct OutputLogs {
    stdout: PathBuf,
    stderr: PathBuf,
}

impl OutputLogs {
    fn create() -> io::Result<(Self, File, File)> {
        loop {
            let id = NEXT_LOG_ID.fetch_add(1, Ordering::Relaxed);
            let prefix = format!("rs-rayon-executor-{}-{id}", std::process::id());
            let stdout = std::env::temp_dir().join(format!("{prefix}.stdout"));
            let stderr = std::env::temp_dir().join(format!("{prefix}.stderr"));
            match Self::create_at(stdout, stderr) {
                Ok(logs) => return Ok(logs),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn create_at(stdout: PathBuf, stderr: PathBuf) -> io::Result<(Self, File, File)> {
        let stdout_file = File::options().write(true).create_new(true).open(&stdout)?;
        match File::options().write(true).create_new(true).open(&stderr) {
            Ok(stderr_file) => Ok((Self { stdout, stderr }, stdout_file, stderr_file)),
            Err(error) => {
                drop(stdout_file);
                let _ = fs::remove_file(&stdout);
                Err(error)
            }
        }
    }

    fn read(&self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        let stdout =
            fs::read(&self.stdout).map_err(|error| with_output_diagnostics("reading child stdout", error, self))?;
        let stderr =
            fs::read(&self.stderr).map_err(|error| with_output_diagnostics("reading child stderr", error, self))?;
        Ok((stdout, stderr))
    }
}

impl Drop for OutputLogs {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.stdout);
        let _ = fs::remove_file(&self.stderr);
    }
}

fn output_summary(path: &PathBuf) -> Option<String> {
    const SUMMARY_LIMIT: u64 = 4096;
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length == 0 {
        return None;
    }
    let start = length.saturating_sub(SUMMARY_LIMIT);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(if start == 0 {
        text.into_owned()
    } else {
        format!("[earlier output omitted]\n{text}")
    })
}

fn with_output_diagnostics(stage: &str, error: io::Error, logs: &OutputLogs) -> io::Error {
    let mut message = format!("{stage}: {error}");
    if let Some(stdout) = output_summary(&logs.stdout) {
        message.push_str(&format!("\nstdout summary:\n{stdout}"));
    }
    if let Some(stderr) = output_summary(&logs.stderr) {
        message.push_str(&format!("\nstderr summary:\n{stderr}"));
    }
    io::Error::new(error.kind(), message)
}

trait ChildProcess {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn kill(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<ExitStatus>;
}

impl ChildProcess for std::process::Child {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        std::process::Child::try_wait(self)
    }

    fn kill(&mut self) -> io::Result<()> {
        std::process::Child::kill(self)
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        std::process::Child::wait(self)
    }
}

fn wait_for_child(
    child: &mut impl ChildProcess,
    timeout: Duration,
    logs: &OutputLogs,
) -> io::Result<(ExitStatus, bool)> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, false)),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let timed_out = match child.kill() {
                    Ok(()) => true,
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => false,
                    Err(error) => {
                        let _ = child.wait();
                        return Err(with_output_diagnostics("killing timed-out child", error, logs));
                    }
                };
                return child
                    .wait()
                    .map(|status| (status, timed_out))
                    .map_err(|error| with_output_diagnostics("waiting for child", error, logs));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(with_output_diagnostics("checking child status", error, logs));
            }
        }
    }
}

/// Runs a child with file-backed output capture and reaps it by the deadline.
///
/// A timeout kills and waits for the direct child. It does not promise to
/// terminate descendants that the child may have started.
///
/// # Returns
///
/// The child status, captured output streams, log paths, and timeout status.
pub(crate) fn run_bounded_output(command: &mut Command, timeout: Duration) -> io::Result<BoundedOutput> {
    let (logs, stdout_file, stderr_file) = OutputLogs::create()?;
    let spawned = command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn();
    // `Command` retains its stdio configuration after spawn. Replace it so
    // the parent handles are closed before reading and removing log files.
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    let mut child = spawned.map_err(|error| with_output_diagnostics("spawning child", error, &logs))?;

    let (status, timed_out) = wait_for_child(&mut child, timeout, &logs)?;

    let (stdout, stderr) = logs.read()?;
    Ok(BoundedOutput {
        output: Output { status, stdout, stderr },
        log_paths: [logs.stdout.clone(), logs.stderr.clone()],
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process::ExitStatus;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::ChildProcess;
    use super::OutputLogs;
    use super::wait_for_child;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn path(label: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rxe-subprocess-{label}-{}-{id}", std::process::id()))
    }

    #[test]
    fn test_stderr_create_collision_closes_and_removes_new_stdout_file() {
        let stdout = path("stdout");
        let stderr = path("stderr");
        fs::write(&stderr, b"pre-existing").expect("create colliding stderr file");

        let result = OutputLogs::create_at(stdout.clone(), stderr.clone());

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("stderr creation should collide"),
        };
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!stdout.exists(), "new stdout file should be removed");
        assert_eq!(fs::read(&stderr).expect("read existing stderr"), b"pre-existing");
        fs::remove_file(stderr).expect("remove test stderr file");
    }

    struct FailingStatusCheck {
        killed: bool,
        waited: bool,
    }

    impl ChildProcess for FailingStatusCheck {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            Err(io::Error::other("injected status failure"))
        }

        fn kill(&mut self) -> io::Result<()> {
            self.killed = true;
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.waited = true;
            Err(io::Error::other("injected reap failure"))
        }
    }

    #[test]
    fn test_status_check_failure_reaps_child_and_preserves_available_output() {
        let (logs, stdout, stderr) = OutputLogs::create().expect("create logs");
        drop(stdout);
        drop(stderr);
        fs::write(&logs.stdout, b"stdout evidence").expect("write stdout evidence");
        fs::write(&logs.stderr, b"stderr evidence").expect("write stderr evidence");
        let mut child = FailingStatusCheck {
            killed: false,
            waited: false,
        };

        let result = wait_for_child(&mut child, Duration::from_secs(1), &logs);

        let error = result.expect_err("injected status check should fail");
        assert!(child.killed, "failed child should be killed");
        assert!(child.waited, "failed child should be reaped");
        let message = error.to_string();
        assert!(message.contains("checking child status"));
        assert!(message.contains("stdout summary:\nstdout evidence"));
        assert!(message.contains("stderr summary:\nstderr evidence"));
    }

    #[test]
    fn test_read_failure_includes_summary_from_readable_stream() {
        let (logs, stdout, stderr) = OutputLogs::create().expect("create logs");
        drop(stdout);
        drop(stderr);
        fs::remove_file(&logs.stdout).expect("remove stdout log to inject read failure");
        fs::write(&logs.stderr, b"stderr diagnostic").expect("write stderr diagnostic");

        let result = logs.read();

        let error = result.expect_err("missing stdout should fail");
        let message = error.to_string();
        assert!(message.contains("reading child stdout"));
        assert!(message.contains("stderr summary:\nstderr diagnostic"));
    }
}
