use crate::adapter::{Adapter, AdapterError, AgentOutput};
use std::path::Path;
use std::process::Command;
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
        }
    }

    /// claude: `claude -p <prompt>` — prints the answer to stdout (headless).
    pub fn claude() -> Self {
        Self {
            name: "claude".into(),
            program: "claude".into(),
            args: vec!["-p".into()],
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
        }
    }

    /// opencode: `opencode run <prompt>`.
    pub fn opencode() -> Self {
        Self {
            name: "opencode".into(),
            program: "opencode".into(),
            args: vec!["run".into()],
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
        }
    }

    /// Override the per-command timeout (e.g. from a future per-agent crew.toml knob).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
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
}

impl Adapter for ExecAdapter {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        use std::io::{Read, Write};
        use std::time::Instant;
        // Pass the prompt via STDIN, not as a CLI arg. On Windows the prompt would otherwise go
        // through `cmd /C <program> ... "<prompt>"`, where cmd's command-line parsing MANGLES
        // multi-line / quoted prompts (newlines are command separators; quoting differs from the
        // CRT rules Rust applies). codex (`exec`), claude (`-p`) and opencode (`run`) all read the
        // prompt from stdin when given no positional prompt — verified on z13.
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
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort: a broken pipe (CLI exited before reading) shows up as empty output
            // below. Dropping `stdin` closes it (EOF), so the CLI stops reading and proceeds.
            let _ = stdin.write_all(prompt.as_bytes());
        }
        // Drain stdout/stderr on dedicated threads. We must NOT let the child's pipe buffers fill
        // while we poll for exit — a reply larger than the OS pipe buffer (~64 KiB) would deadlock
        // the child (blocked on write) against us (blocked on wait). The readers EOF when the child
        // exits or is killed (pipes close), so joining them is always bounded.
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

        // Poll for exit until the per-command deadline. A turn that never returns is killed and
        // reported Flaked rather than hanging the whole run (the conductor's wall-clock budget only
        // fires at round boundaries, so it cannot rescue a single wedged `run()`).
        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        // On Windows the child is `cmd /C <program>`; killing cmd does NOT reap the
                        // grandchild CLI, so taskkill the whole tree by pid. (On Unix we exec the
                        // program directly — `kill` is sufficient.)
                        #[cfg(windows)]
                        {
                            let _ = Command::new("taskkill")
                                .args(["/F", "/T", "/PID", &child.id().to_string()])
                                .output();
                        }
                        let _ = child.wait(); // reap so the readers' pipes close
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
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AdapterError::Flaked(e.to_string()));
                }
            }
        }
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
            if err_low.contains("rate") || err.contains("429") {
                return Err(AdapterError::RateLimited);
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
        let a = ExecAdapter::raw("sleeper", "ping", &["-n", "8", "127.0.0.1"], timeout);
        #[cfg(not(windows))]
        let a = ExecAdapter::raw("sleeper", "sleep", &["6"], timeout);

        let start = Instant::now();
        let r = a.run("ignored", Path::new("."));
        let elapsed = start.elapsed();
        assert!(
            matches!(r, Err(AdapterError::Flaked(_))),
            "a timed-out turn must Flake, got {r:?}"
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "must kill near the timeout, not wait out the child: {elapsed:?}"
        );
    }

    #[test]
    fn run_captures_quick_output_within_timeout() {
        // Regression guard for the reader-thread rewrite: a fast command's stdout is still captured.
        let a = ExecAdapter::raw("echoer", "echo", &["hello-ensemble"], Duration::from_secs(10));
        let r = a.run("", Path::new(".")).expect("echo should succeed");
        assert!(r.text.contains("hello-ensemble"), "captured {:?}", r.text);
    }
}
