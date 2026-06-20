//! The automated test gate (firewall A): run a project's real test command in the worktree; GREEN
//! (exit 0) unlocks the AI reviewers, RED bounces the traceback back to the implementer. A test that
//! can't even be RUN is treated as RED (fail-closed — never let an un-runnable suite look green).

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct TestOutcome {
    pub passed: bool,
    pub output: String, // excerpted combined stdout+stderr (the tail — failures live at the end)
}

fn red(output: String) -> TestOutcome {
    TestOutcome {
        passed: false,
        output,
    }
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

/// Drain a child pipe into a SHARED byte sink on its own thread. Draining concurrently is essential
/// under a timeout: a verbose suite that fills the OS pipe buffer would otherwise block on `write()`
/// forever (nobody reading) and be falsely timed out. Appending into a shared sink (rather than
/// returning at EOF) lets the timeout path SNAPSHOT whatever was drained so far WITHOUT joining — so
/// a grandchild that inherits the pipe and outlives the killed shell can't make the timeout hang.
fn spawn_drain<R: Read + Send + 'static>(
    pipe: Option<R>,
    sink: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if let Some(mut p) = pipe {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = p.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if let Ok(mut g) = sink.lock() {
                    g.extend_from_slice(&chunk[..n]);
                }
            }
        }
    })
}

/// Kill the timed-out test command (best-effort). NOTE: this kills the shell process; grandchildren
/// it spawned (e.g. `cargo test`'s test binaries) may linger until they finish on their own — a true
/// process-tree kill (Unix process-group / Windows job object) is a documented follow-up. A
/// negative-pid group kill was tried and removed: it is unsafe inside a test harness (it can reap
/// the caller's own group).
fn kill_command(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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
        Err(e) => return red(format!("could not run test command `{command}`: {e}")),
    };

    let Some(secs) = timeout_secs else {
        // No timeout: `wait_with_output` drains both pipes while it waits — no deadlock possible.
        return match child.wait_with_output() {
            Ok(o) => {
                let mut combined = String::from_utf8_lossy(&o.stdout).into_owned();
                combined.push_str(&String::from_utf8_lossy(&o.stderr));
                TestOutcome {
                    passed: o.status.success(),
                    output: tail(&combined, 2000),
                }
            }
            Err(e) => red(format!("test command failed: {e}")),
        };
    };

    // Timeout path: drain stdout+stderr on threads into a shared sink so a verbose-but-passing suite
    // can't fill the pipe buffer, block, and be falsely timed out.
    let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
    let ho = spawn_drain(child.stdout.take(), sink.clone());
    let he = spawn_drain(child.stderr.take(), sink.clone());
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut exit_ok = None;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_ok = Some(status.success());
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    kill_command(&mut child);
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                kill_command(&mut child);
                return red(format!("test command wait failed: {e}"));
            }
        }
    }
    // On a NORMAL exit the pipes are closing, so join briefly to capture the full tail. On TIMEOUT we
    // deliberately do NOT join: a surviving grandchild may hold the pipe open, which would hang the
    // join past the deadline — instead snapshot whatever was drained so far so the timeout is bounded.
    if !timed_out {
        let _ = ho.join();
        let _ = he.join();
    }
    let combined = String::from_utf8_lossy(&sink.lock().unwrap()).into_owned();
    if timed_out {
        return red(format!(
            "test command timed out after {secs}s\n{}",
            tail(&combined, 2000)
        ));
    }
    TestOutcome {
        passed: exit_ok.unwrap_or(false),
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
        assert!(run_tests(dir.path(), "exit 0", None).passed);
    }

    #[test]
    fn red_on_exit_one() {
        // `exit 1` is non-zero in both `sh` and `cmd` → cross-platform RED.
        let dir = tempfile::tempdir().unwrap();
        assert!(!run_tests(dir.path(), "exit 1", None).passed);
    }

    #[cfg(unix)]
    #[test]
    fn red_captures_stderr_output() {
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

    #[cfg(unix)]
    #[test]
    fn large_output_within_timeout_is_green_not_falsely_timed_out() {
        // ~200KB of output (well over the pipe buffer) that exits 0 within the timeout. With the
        // concurrent pipe-drain this is GREEN; without it the child would block on a full pipe and
        // be falsely killed as "timed out". Regression test for that bug.
        let dir = tempfile::tempdir().unwrap();
        let t = run_tests(
            dir.path(),
            "for i in $(seq 1 8000); do echo xxxxxxxxxxxxxxxxxxxxxxxx; done; exit 0",
            Some(30),
        );
        assert!(
            t.passed,
            "a verbose but passing suite must be GREEN, not falsely timed out"
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_is_bounded_even_if_a_child_holds_the_pipe() {
        // A backgrounded `sleep` inherits the pipe and OUTLIVES the killed shell. The timeout must
        // still return promptly (snapshot, don't join) instead of hanging until that child exits.
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let t = run_tests(dir.path(), "sleep 8 & sleep 8", Some(1));
        assert!(!t.passed);
        assert!(t.output.contains("timed out"));
        assert!(
            start.elapsed().as_secs() < 5,
            "timeout must be bounded, took {:?}",
            start.elapsed()
        );
    }
}
