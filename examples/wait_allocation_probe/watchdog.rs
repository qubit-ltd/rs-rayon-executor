// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

//! Parent-process supervision for the allocation probe.

use std::io;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::thread;
use std::time::Duration;
use std::time::Instant;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn run(command: &mut Command, timeout: Duration) -> Result<ExitStatus, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawning probe child: {error}"))?;
    let child_id = child.id();
    let status = wait_for_child(&mut child, timeout).map_err(|error| format!("{error}; child id {child_id}"))?;
    if status.success() {
        Ok(status)
    } else {
        Err(format!("probe child {child_id} exited unsuccessfully ({status})"))
    }
}

fn wait_for_child(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let started = Instant::now();
    supervise(child, timeout, || started.elapsed(), thread::sleep)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FakeStatus(bool);

trait ChildOps {
    type Status;

    fn try_wait(&mut self) -> io::Result<Option<Self::Status>>;
    fn kill(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<Self::Status>;
}

impl ChildOps for Child {
    type Status = ExitStatus;

    fn try_wait(&mut self) -> io::Result<Option<Self::Status>> {
        Child::try_wait(self)
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn wait(&mut self) -> io::Result<Self::Status> {
        Child::wait(self)
    }
}

#[cfg(test)]
impl std::fmt::Display for FakeStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "success={}", self.0)
    }
}

fn supervise<C, Now, Sleep>(
    child: &mut C,
    timeout: Duration,
    mut now: Now,
    mut sleep: Sleep,
) -> Result<C::Status, String>
where
    C: ChildOps,
    C::Status: std::fmt::Display,
    Now: FnMut() -> Duration,
    Sleep: FnMut(Duration),
{
    let started = now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if now().saturating_sub(started) < timeout => sleep(POLL_INTERVAL),
            Ok(None) => {
                let kill_error = child.kill().err();
                if let Some(error) = &kill_error {
                    // Child::kill can report InvalidInput when the process exited
                    // between try_wait and kill. Observe that status before
                    // classifying the watchdog as a timeout.
                    if error.kind() == io::ErrorKind::InvalidInput {
                        match child.try_wait() {
                            Ok(Some(status)) => return Ok(status),
                            Ok(None) => {}
                            Err(check_error) => {
                                let reap = child.wait().err();
                                return Err(format!(
                                    "checking child status after timeout kill race: {check_error}{}",
                                    reap.map_or_else(String::new, |error| format!("; waiting for child: {error}"))
                                ));
                            }
                        }
                    }
                }
                let wait_result = child.wait();
                let message = format!("probe child timed out after {timeout:?}");
                return match (kill_error, wait_result) {
                    (None, Ok(status)) => Err(format!("{message}; killed and waited for child ({status})")),
                    (Some(kill), Ok(status)) => {
                        Err(format!("{message}; killing child failed: {kill}; waited ({status})"))
                    }
                    (None, Err(wait)) => Err(format!("{message}; child killed but waiting failed: {wait}")),
                    (Some(kill), Err(wait)) => Err(format!(
                        "{message}; killing child failed: {kill}; waiting failed: {wait}"
                    )),
                };
            }
            Err(error) => {
                let kill = child.kill().err();
                let wait = child.wait().err();
                return Err(format!(
                    "checking child status failed: {error}{}{}",
                    kill.map_or_else(String::new, |error| format!("; killing child failed: {error}")),
                    wait.map_or_else(String::new, |error| format!("; waiting for child failed: {error}")),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use super::ChildOps;
    use super::FakeStatus;
    use super::supervise;

    struct FakeChild {
        states: Vec<io::Result<Option<FakeStatus>>>,
        killed: bool,
        waited: bool,
        kill_error: Option<io::ErrorKind>,
        wait_error: bool,
    }

    impl ChildOps for FakeChild {
        type Status = FakeStatus;

        fn try_wait(&mut self) -> io::Result<Option<Self::Status>> {
            self.states.remove(0)
        }

        fn kill(&mut self) -> io::Result<()> {
            self.killed = true;
            match self.kill_error {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok(()),
            }
        }

        fn wait(&mut self) -> io::Result<Self::Status> {
            self.waited = true;
            if self.wait_error {
                Err(io::Error::other("injected wait failure"))
            } else {
                Ok(FakeStatus(false))
            }
        }
    }

    fn fake(states: Vec<io::Result<Option<FakeStatus>>>) -> FakeChild {
        FakeChild {
            states,
            killed: false,
            waited: false,
            kill_error: None,
            wait_error: false,
        }
    }

    fn advancing_clock() -> impl FnMut() -> Duration {
        let mut elapsed = Duration::ZERO;
        move || {
            let observed = elapsed;
            elapsed += Duration::from_millis(10);
            observed
        }
    }

    #[test]
    fn test_deterministic_success_does_not_kill_child() {
        let mut child = fake(vec![Ok(Some(FakeStatus(true)))]);
        let status = supervise(&mut child, Duration::from_secs(1), || Duration::ZERO, |_| {})
            .expect("success should return status");
        assert_eq!(status, FakeStatus(true));
        assert!(!child.killed);
        assert!(!child.waited);
    }

    #[test]
    fn test_deterministic_non_success_status_is_returned() {
        let mut child = fake(vec![Ok(Some(FakeStatus(false)))]);
        let status = supervise(&mut child, Duration::from_secs(1), || Duration::ZERO, |_| {})
            .expect("failure status should be returned");
        assert_eq!(status, FakeStatus(false));
    }

    #[test]
    fn test_deterministic_timeout_kills_and_waits() {
        let mut child = fake(vec![Ok(None)]);
        let error = supervise(&mut child, Duration::from_millis(10), advancing_clock(), |_| {})
            .expect_err("child should time out");
        assert!(error.contains("timed out"));
        assert!(child.killed);
        assert!(child.waited);
    }

    #[test]
    fn test_deterministic_status_error_kills_and_waits() {
        let mut child = fake(vec![Err(io::Error::other("injected check failure"))]);
        let error = supervise(&mut child, Duration::from_secs(1), || Duration::ZERO, |_| {})
            .expect_err("status check failure should be reported");
        assert!(error.contains("checking child status"));
        assert!(error.contains("injected check failure"));
        assert!(child.killed);
        assert!(child.waited);
    }

    #[test]
    fn test_deterministic_kill_error_is_reported_and_reap_is_attempted() {
        let mut child = fake(vec![Ok(None)]);
        child.kill_error = Some(io::ErrorKind::PermissionDenied);
        let error = supervise(&mut child, Duration::from_millis(10), advancing_clock(), |_| {})
            .expect_err("failed kill should be diagnostic");
        assert!(error.contains("killing child failed"));
        assert!(child.waited, "wait is best effort after kill failure");
    }

    #[test]
    fn test_deterministic_wait_error_is_reported() {
        let mut child = fake(vec![Ok(None)]);
        child.wait_error = true;
        let error = supervise(&mut child, Duration::from_millis(10), advancing_clock(), |_| {})
            .expect_err("failed wait should be diagnostic");
        assert!(error.contains("waiting failed"));
        assert!(error.contains("injected wait failure"));
    }

    #[test]
    fn test_deterministic_exit_race_is_rechecked() {
        let mut child = fake(vec![Ok(None), Ok(Some(FakeStatus(false)))]);
        child.kill_error = Some(io::ErrorKind::InvalidInput);
        let status = supervise(&mut child, Duration::from_millis(10), advancing_clock(), |_| {})
            .expect("racing exit should return the actual status");
        assert_eq!(status, FakeStatus(false));
        assert!(!child.waited);
    }
}
