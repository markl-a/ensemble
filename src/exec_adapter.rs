use crate::adapter::{detect_rate_limit, Adapter, AdapterError, AgentOutput};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Per-command wall-clock ceiling for ONE CLI turn. A turn that exceeds this is treated as wedged
/// (the child is killed → `Flaked`) instead of hanging the whole run. Generous by default — real
/// implementer turns can take minutes — but the conductor's `max_task_secs` only fires at round
/// BOUNDARIES, so without this a CLI that never returns from a single `run()` would hang forever
/// (observed with opencode). Overridable via `with_timeout`.
pub const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 600;

/// Drives a vendor CLI headlessly by exec: `program <args...>` with the prompt on stdin, capturing
/// stdout as the reply. Per-vendor invocation contracts live in the constructors (design §4a).
pub struct ExecAdapter {
    name: String,
    program: String,
    args: Vec<String>,
    /// Per-command timeout (see `DEFAULT_EXEC_TIMEOUT_SECS`).
    timeout: Duration,
    /// S1b: an optional hard-abort flag (set by the conductor's `set_abort`). When flipped DURING a
    /// `run()`, the poll loop kills the child and Flakes — so `ensemble abort --hard` is immediate.
    abort: Mutex<Option<Arc<AtomicBool>>>,
}

impl ExecAdapter {
    /// codex: `codex exec --skip-git-repo-check "<prompt>"`. (Phase-1 parses the final text from
    /// stdout; refine structured `--json` parsing in Phase 1b.)
    pub fn codex() -> Self {
        Self {
            name: "codex".into(),
            program: "codex".into(),
            // The implementer must actually edit files autonomously, so bypass approvals/sandbox
            // (it runs inside an isolated, throwaway git worktree). --skip-git-repo-check lets it
            // run in the worktree without a git-root prompt.
            args: vec![
                "exec".into(),
                "--dangerously-bypass-approvals-and-sandbox".into(),
                "--skip-git-repo-check".into(),
            ],
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
            abort: Mutex::new(None),
        }
    }

    /// claude: `claude -p <prompt>` — prints the answer to stdout (headless).
    pub fn claude() -> Self {
        Self {
            name: "claude".into(),
            program: "claude".into(),
            args: vec!["-p".into()],
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
            abort: Mutex::new(None),
        }
    }

    /// opencode: `opencode run <prompt>`.
    pub fn opencode() -> Self {
        Self {
            name: "opencode".into(),
            program: "opencode".into(),
            args: vec!["run".into()],
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
            abort: Mutex::new(None),
        }
    }

    /// Override the per-command timeout (e.g. from a `[agents.<n>] timeout = N` crew.toml knob).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Append extra CLI args (item 6 — `[agents.<n>] args = [...]`) after the vendor's base args, e.g.
    /// `["--model", "gpt-5.5"]`. They precede the stdin-delivered prompt, matching each CLI's contract.
    pub fn with_extra_args(mut self, extra: Vec<String>) -> Self {
        self.args.extend(extra);
        self
    }
}

impl ExecAdapter {
    /// Build the base command. On Windows the vendor CLIs are typically npm-global `.cmd` shims
    /// (codex/opencode) that `CreateProcess` cannot exec directly, so route through `cmd /C
    /// <program> <args...>`; on Unix, exec the program directly. (Same lesson as a real PTY/CLI
    /// driver — npm shims need a shell on Windows.)
    #[cfg(windows)]
    fn build_command(&self) -> Command {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(&self.program).args(&self.args);
        c
    }
    #[cfg(not(windows))]
    fn build_command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c
    }

    /// Kill the timed-out child so no survivor keeps the captured pipes open (which would hang the
    /// reader joins). Windows: the child is `cmd /C <program>`; `taskkill /F /T` walks the tree from
    /// the STILL-LIVE cmd pid FIRST — killing cmd before taskkill would orphan the grandchild CLI
    /// and break the enumeration — then `child.kill()` cleans up cmd itself. Unix: we exec the
    /// program directly, so `child.kill()` reaps it; a CLI that forks a helper which inherits and
    /// holds the stdout pipe could still delay the reader joins (a documented limitation — a
    /// process-group kill was tried but misbehaved under the test harness).
    fn kill_tree(child: &mut std::process::Child) {
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &child.id().to_string()])
                .output();
        }
        let _ = child.kill();
    }
}

impl Adapter for ExecAdapter {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_abort(&self, flag: Arc<AtomicBool>) {
        *self.abort.lock().unwrap() = Some(flag);
    }
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        use std::io::{Read, Write};
        use std::time::Instant;
        // Snapshot the hard-abort flag for this turn (S1b): set mid-run ⇒ kill the child and Flake.
        let abort = self.abort.lock().unwrap().clone();
        // Pass the prompt via STDIN, not as a CLI arg. On Windows the prompt would otherwise go
        // through `cmd /C <program> ... "<prompt>"`, where cmd's command-line parsing MANGLES
        // multi-line / quoted prompts (newlines are command separators; quoting differs from the
        // CRT rules Rust applies). codex (`exec`), claude (`-p`) and opencode (`run`) all read the
        // prompt from stdin when given no positional prompt — verified locally.
        let mut child = match self
            .build_command()
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AdapterError::NotInstalled(self.program.clone()))
            }
            Err(e) => return Err(AdapterError::Flaked(e.to_string())),
        };
        // Drain stdout/stderr on dedicated threads, STARTED BEFORE the stdin write below — we must
        // NOT let the child's pipe buffers fill while anyone blocks: a reply larger than the OS pipe
        // buffer (~64 KiB) would otherwise deadlock the child (blocked on write) against us. The
        // readers EOF only when the child AND every descendant holding the write end have exited or
        // been killed (`kill_tree`). On the common path the join is deadline-bounded (Windows kills
        // the whole tree; the direct child on Unix); a Unix-forked helper that inherits and keeps
        // the pipe open could still delay the join — the residual limitation noted on `kill_tree`.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stdout {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stderr {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        // Deliver the prompt on a WRITER thread so a CLI that never drains stdin cannot block the
        // main thread before it reaches the deadline loop. Dropping `stdin` (on completion, or when
        // the child is killed and the write fails with a broken pipe) closes it → EOF for the CLI.
        let stdin = child.stdin.take();
        let prompt_bytes = prompt.as_bytes().to_vec();
        let writer = std::thread::spawn(move || {
            if let Some(mut si) = stdin {
                let _ = si.write_all(&prompt_bytes);
            }
        });

        // Poll for exit until the per-command deadline. A turn that never returns is killed and
        // reported Flaked rather than hanging the whole run (the conductor's wall-clock budget only
        // fires at round boundaries, so it cannot rescue a single wedged `run()`).
        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) => {
                    // S1b `--hard`: an operator hard-abort flips this flag — kill the running CLI now
                    // (don't wait for the timeout or the round boundary) and report it Flaked.
                    if abort.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
                        Self::kill_tree(&mut child);
                        let _ = child.wait();
                        let _ = writer.join();
                        let _ = out_reader.join();
                        let _ = err_reader.join();
                        return Err(AdapterError::Flaked(format!(
                            "{} aborted by operator",
                            self.program
                        )));
                    }
                    if Instant::now() >= deadline {
                        Self::kill_tree(&mut child);
                        let _ = child.wait(); // reap so the readers' pipes close
                        let _ = writer.join();
                        let _ = out_reader.join();
                        let _ = err_reader.join();
                        return Err(AdapterError::Flaked(format!(
                            "{} timed out after {}s",
                            self.program,
                            self.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    Self::kill_tree(&mut child);
                    let _ = child.wait();
                    let _ = writer.join();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(AdapterError::Flaked(e.to_string()));
                }
            }
        }
        let _ = writer.join();
        let stdout_buf = out_reader.join().unwrap_or_default();
        let stderr_buf = err_reader.join().unwrap_or_default();
        let text = String::from_utf8_lossy(&stdout_buf).trim().to_string();
        if text.is_empty() {
            // When shelled through `cmd /C`, a missing CLI is a non-zero exit + a "not recognized"
            // stderr (not a spawn NotFound), so classify that here.
            let err = String::from_utf8_lossy(&stderr_buf);
            let err_low = err.to_lowercase();
            if err_low.contains("not recognized") || err_low.contains("cannot find") {
                return Err(AdapterError::NotInstalled(self.program.clone()));
            }
            // Quota/rate-limit: vendors print this to EITHER stream and may exit 0 (codex prints
            // "You've hit your usage limit … try again at <when>" with an empty answer), so scan the
            // combined output and preserve the reason + reset time instead of a bare `Empty`.
            let combined = format!("{}\n{}", String::from_utf8_lossy(&stdout_buf), err);
            if let Some(info) = detect_rate_limit(&combined) {
                return Err(AdapterError::RateLimited(info));
            }
            return Err(AdapterError::Empty);
        }
        Ok(AgentOutput {
            agent: self.name.clone(),
            text,
        })
    }
}

#[cfg(test)]
impl ExecAdapter {
    /// Construct an adapter pointed at an arbitrary program — for hermetic timeout/output tests.
    fn raw(name: &str, program: &str, args: &[&str], timeout: Duration) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            timeout,
            abort: Mutex::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn names_and_programs() {
        assert_eq!(ExecAdapter::codex().name(), "codex");
        assert_eq!(ExecAdapter::claude().name(), "claude");
        assert_eq!(ExecAdapter::opencode().name(), "opencode");
    }

    #[test]
    fn run_times_out_and_flakes_instead_of_hanging() {
        // A CLI turn that outlasts the timeout must be killed and reported Flaked — not block the
        // whole run. (`sleep`/`ping` are the portable "block for a while" primitives.)
        let timeout = Duration::from_millis(500);
        #[cfg(windows)]
        let a = ExecAdapter::raw("sleeper", "ping", &["-n", "30", "127.0.0.1"], timeout);
        #[cfg(not(windows))]
        let a = ExecAdapter::raw("sleeper", "sleep", &["6"], timeout);

        let start = Instant::now();
        let r = a.run("ignored", Path::new("."));
        let elapsed = start.elapsed();
        assert!(
            matches!(r, Err(AdapterError::Flaked(_))),
            "a timed-out turn must Flake, got {r:?}"
        );
        #[cfg(windows)]
        let limit = Duration::from_secs(15);
        #[cfg(not(windows))]
        let limit = Duration::from_secs(4);
        assert!(
            elapsed < limit,
            "must kill near the timeout, not wait out the child: {elapsed:?}"
        );
    }

    #[test]
    fn run_captures_quick_output_within_timeout() {
        // Regression guard for the reader-thread rewrite: a fast command's stdout is still captured.
        let a = ExecAdapter::raw(
            "echoer",
            "echo",
            &["hello-ensemble"],
            Duration::from_secs(10),
        );
        let r = a.run("", Path::new(".")).expect("echo should succeed");
        assert!(r.text.contains("hello-ensemble"), "captured {:?}", r.text);
    }

    #[test]
    fn with_extra_args_appends_to_the_invocation() {
        // item 6: `echo hi` + extra ["world"] → `echo hi world` → the appended arg reaches the CLI.
        let a = ExecAdapter::raw("echoer", "echo", &["hi"], Duration::from_secs(10))
            .with_extra_args(vec!["world".to_string()]);
        let r = a.run("", Path::new(".")).expect("echo should succeed");
        assert!(
            r.text.contains("hi") && r.text.contains("world"),
            "captured {:?}",
            r.text
        );
    }

    #[cfg(unix)]
    #[test]
    fn quota_on_stderr_with_empty_stdout_classifies_as_rate_limited() {
        // The real failure mode: codex exits 0 with an EMPTY answer and prints the usage-limit line
        // to stderr. That must degrade as RateLimited (with the reset time), not a bare Empty.
        let script = "echo \"ERROR: You've hit your usage limit. Visit x or try again at \
                      Jun 25th, 2026 5:33 AM.\" 1>&2";
        let a = ExecAdapter::raw("codex", "sh", &["-c", script], Duration::from_secs(10));
        match a.run("ignored", Path::new(".")) {
            Err(AdapterError::RateLimited(info)) => {
                assert_eq!(info.retry_at.as_deref(), Some("Jun 25th, 2026 5:33 AM"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn external_abort_kills_a_running_turn_mid_flight() {
        // S1b `--hard`: a long turn that the operator hard-aborts must die NEAR the abort, not run to
        // its (generous) timeout. Set a 30s timeout, abort after ~300ms, expect Flaked in well under 3s.
        #[cfg(windows)]
        let a = ExecAdapter::raw(
            "sleeper",
            "ping",
            &["-n", "30", "127.0.0.1"],
            Duration::from_secs(30),
        );
        #[cfg(not(windows))]
        let a = ExecAdapter::raw("sleeper", "sleep", &["30"], Duration::from_secs(30));
        let flag = Arc::new(AtomicBool::new(false));
        a.set_abort(flag.clone());
        let f2 = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            f2.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        let r = a.run("ignored", Path::new("."));
        assert!(
            matches!(r, Err(AdapterError::Flaked(_))),
            "an aborted turn must Flake, got {r:?}"
        );
        #[cfg(windows)]
        let limit = Duration::from_secs(10);
        #[cfg(not(windows))]
        let limit = Duration::from_secs(3);
        assert!(
            start.elapsed() < limit,
            "must die near the abort: {:?}",
            start.elapsed()
        );
    }
}
