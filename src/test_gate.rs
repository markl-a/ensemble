//! The automated test gate (firewall A): run a project's real test command in the worktree; GREEN
//! (exit 0) unlocks the AI reviewers, RED bounces the traceback back to the implementer. A test that
//! can't even be RUN is treated as RED (fail-closed — never let an un-runnable suite look green).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct TestOutcome {
    pub passed: bool,
    pub output: String, // excerpted combined stdout+stderr (the tail — failures live at the end)
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(command);
    c
}
#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(command);
    c
}

/// Run `command` in `worktree`. Returns RED on non-zero exit, on a spawn failure, or on timeout.
pub fn run_tests(worktree: &Path, command: &str, timeout_secs: Option<u64>) -> TestOutcome {
    let mut cmd = shell(command);
    cmd.current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return TestOutcome {
                passed: false,
                output: format!("could not run test command `{command}`: {e}"),
            }
        }
    };
    if let Some(secs) = timeout_secs {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return TestOutcome {
                            passed: false,
                            output: format!("test command timed out after {secs}s"),
                        };
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return TestOutcome {
                        passed: false,
                        output: format!("test command wait failed: {e}"),
                    }
                }
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return TestOutcome {
                passed: false,
                output: format!("test command failed: {e}"),
            }
        }
    };
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    TestOutcome {
        passed: out.status.success(),
        output: tail(&combined, 2000),
    }
}

/// Keep the LAST `max` chars (test failures/tracebacks are at the end of the output).
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let skip = n - max;
        format!("…{}", s.chars().skip(skip).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_on_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let t = run_tests(dir.path(), "exit 0", None);
        assert!(t.passed);
    }

    #[test]
    fn red_on_nonzero_with_output() {
        let dir = tempfile::tempdir().unwrap();
        let t = run_tests(dir.path(), "echo boom 1>&2; exit 1", None);
        assert!(!t.passed);
        assert!(t.output.contains("boom"));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_is_red() {
        let dir = tempfile::tempdir().unwrap();
        let t = run_tests(dir.path(), "sleep 5", Some(1));
        assert!(!t.passed);
        assert!(t.output.contains("timed out"));
    }
}
