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

/// Environment variable that pins the binary `ExecAdapter::spectyn()` runs (same pattern as the
/// launcher's `ENSEMBLE_<CLIENT>_BIN`). Unset or blank → `spectyn` on PATH.
pub const SPECTYN_BIN_ENV: &str = "ENSEMBLE_SPECTYN_BIN";

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

    /// spectyn: `spectyn exec --agent coder` — reads the prompt on stdin, prints the answer on stdout
    /// (exit 0 ok / 1 agent error / 2 bad args). `ENSEMBLE_SPECTYN_BIN` overrides the binary.
    pub fn spectyn() -> Self {
        Self::spectyn_with_bin(std::env::var(SPECTYN_BIN_ENV).ok())
    }

    /// `spectyn()` with the override passed in, so tests need not mutate the process env.
    fn spectyn_with_bin(bin: Option<String>) -> Self {
        let program = bin
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "spectyn".to_string());
        Self {
            name: "spectyn".into(),
            program,
            args: vec!["exec".into(), "--agent".into(), "coder".into()],
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
            abort: Mutex::new(None),
        }
    }

    /// The program this adapter execs (a bare PATH name, or the `ENSEMBLE_SPECTYN_BIN` override).
    /// `ensemble serve` probes it so a node only advertises the vendors it can actually run.
    pub fn program(&self) -> &str {
        &self.program
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
    /// driver — npm shims need a shell on Windows.) Exception: an absolute path to an existing
    /// `.exe` (e.g. `ENSEMBLE_SPECTYN_BIN=C:\Program Files\spectyn\spectyn.exe`) is spawned
    /// directly — it needs no shell, and `cmd /C` would split a path containing spaces.
    #[cfg(windows)]
    fn build_command(&self) -> Command {
        if is_direct_exe(Path::new(&self.program)) {
            let mut c = Command::new(&self.program);
            c.args(&self.args);
            return c;
        }
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
    /// reader joins). Windows: the child is usually `cmd /C <program>`; `taskkill /F /T` walks the
    /// tree from the STILL-LIVE cmd pid FIRST — killing cmd before taskkill would orphan the
    /// grandchild CLI and break the enumeration — then `child.kill()` cleans up cmd itself. Unix:
    /// we exec the program directly, so `child.kill()` reaps it; a CLI that forks a helper which inherits and
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

/// Windows: true when `program` can be handed straight to `CreateProcess` — an absolute path to an
/// existing `.exe` file. Bare names (`codex`, `opencode`) may resolve to `.cmd`/`.bat` npm shims and
/// keep going through `cmd /C`.
#[cfg(windows)]
fn is_direct_exe(program: &Path) -> bool {
    program.is_absolute()
        && program
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        && program.is_file()
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
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
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
        };
        let _ = writer.join();
        let stdout_buf = out_reader.join().unwrap_or_default();
        let stderr_buf = err_reader.join().unwrap_or_default();
        let text = String::from_utf8_lossy(&stdout_buf).trim().to_string();
        let err = String::from_utf8_lossy(&stderr_buf);
        if text.is_empty() {
            // When shelled through `cmd /C`, a missing CLI is a non-zero exit + a "not recognized"
            // stderr (not a spawn NotFound), so classify that here.
            let err_low = err.to_lowercase();
            if err_low.contains("not recognized") || err_low.contains("cannot find") {
                return Err(AdapterError::NotInstalled(self.program.clone()));
            }
        }
        // Quota/rate-limit: vendors print this to EITHER stream and may exit 0 (codex prints
        // "You've hit your usage limit … try again at <when>" with an empty answer) or non-zero, so
        // scan the combined output and preserve the reason + reset time instead of a bare `Empty` /
        // `Flaked`. An exit-0 turn WITH an answer is not scanned: the answer may merely mention quota.
        if text.is_empty() || !status.success() {
            let combined = format!("{}\n{}", String::from_utf8_lossy(&stdout_buf), err);
            if let Some(info) = detect_rate_limit(&combined) {
                return Err(AdapterError::RateLimited(info));
            }
        }
        // A non-zero exit is a failed turn even when the CLI printed something (spectyn exits 1 on an
        // agent error or a cut-off answer, codex on failures): that text is an error message, and
        // counting it as a reply could even turn it into a verdict.
        if !status.success() {
            let code = status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"));
            let tail = if err.trim().is_empty() {
                text.as_str()
            } else {
                err.trim()
            };
            return Err(AdapterError::Flaked(format!(
                "{} exited with {code}: {}",
                self.program,
                tail_chars(tail, EXIT_TAIL_CHARS)
            )));
        }
        if text.is_empty() {
            return Err(AdapterError::Empty);
        }
        Ok(AgentOutput {
            agent: self.name.clone(),
            text,
        })
    }
}

/// How much of a failed turn's stderr (or stdout, when stderr is empty) the `Flaked` reason keeps.
const EXIT_TAIL_CHARS: usize = 300;

/// The last `max` chars of `s` (char-boundary safe), prefixed with `…` when cut.
fn tail_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let cut: String = s.chars().skip(n - max).collect();
    format!("…{cut}")
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
        assert_eq!(ExecAdapter::spectyn().name(), "spectyn");
    }

    #[test]
    fn spectyn_runs_exec_agent_coder_and_honours_the_bin_override() {
        let a = ExecAdapter::spectyn_with_bin(None);
        assert_eq!(a.program, "spectyn");
        assert_eq!(a.args, vec!["exec", "--agent", "coder"]);
        let a = ExecAdapter::spectyn_with_bin(Some(" C:/bin/spectyn.exe ".into()));
        assert_eq!(a.program, "C:/bin/spectyn.exe");
        assert_eq!(a.name(), "spectyn");
        // a blank override falls back to PATH instead of running an empty program name
        let a = ExecAdapter::spectyn_with_bin(Some("  ".into()));
        assert_eq!(a.program, "spectyn");
    }

    #[cfg(windows)]
    #[test]
    fn spaced_absolute_exe_is_spawned_directly_not_through_cmd() {
        // `cmd /C C:\Program Files\...\spectyn.exe` splits at the space; an existing absolute .exe
        // must become the Command's program itself, with the vendor args untouched.
        let dir = tempfile::Builder::new()
            .prefix("ensemble dir with spaces ")
            .tempdir()
            .unwrap();
        let exe = dir.path().join("spectyn.exe");
        std::fs::write(&exe, b"").unwrap();
        let a = ExecAdapter::spectyn_with_bin(Some(exe.to_string_lossy().into_owned()));
        let c = a.build_command();
        assert_eq!(c.get_program(), exe.as_os_str());
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["exec", "--agent", "coder"]);
    }

    #[cfg(windows)]
    #[test]
    fn bare_name_still_goes_through_cmd() {
        // npm-global `.cmd` shims (codex/opencode) need the shell; so does a non-existent .exe path.
        let a = ExecAdapter::codex();
        let c = a.build_command();
        assert_eq!(c.get_program(), "cmd");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(&args[..2], ["/C", "codex"]);
        assert_eq!(args[2], "exec");
        let missing = ExecAdapter::spectyn_with_bin(Some(r"C:\no such dir\spectyn.exe".into()));
        assert_eq!(missing.build_command().get_program(), "cmd");
    }

    #[cfg(windows)]
    #[test]
    fn run_executes_an_exe_under_a_spaced_path() {
        // End to end: a real exe in a directory with spaces runs and its stdout is captured.
        let dir = tempfile::Builder::new()
            .prefix("ensemble dir with spaces ")
            .tempdir()
            .unwrap();
        let exe = dir.path().join("hostname.exe");
        std::fs::copy(r"C:\Windows\System32\hostname.exe", &exe).unwrap();
        let a = ExecAdapter::raw(
            "spaced",
            &exe.to_string_lossy(),
            &[],
            Duration::from_secs(10),
        );
        let r = a.run("", Path::new(".")).expect("spaced exe should run");
        assert!(!r.text.is_empty(), "captured {:?}", r.text);
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
        let limit = Duration::from_secs(25);
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

    /// A tiny fake CLI running `script` in the OS shell (cmd on Windows, sh elsewhere).
    fn shell(name: &str, script: &str) -> ExecAdapter {
        #[cfg(windows)]
        let (program, flag) = ("cmd", "/C");
        #[cfg(not(windows))]
        let (program, flag) = ("sh", "-c");
        ExecAdapter::raw(name, program, &[flag, script], Duration::from_secs(10))
    }

    #[test]
    fn non_zero_exit_with_output_flakes_with_code_and_stderr_tail() {
        // spectyn exits 1 on an agent error and still prints text: that text is NOT a reply.
        #[cfg(windows)]
        let a = shell("spectyn", "echo hi & echo boom-on-stderr 1>&2 & exit /b 3");
        #[cfg(not(windows))]
        let a = shell("spectyn", "echo hi; echo boom-on-stderr 1>&2; exit 3");
        match a.run("ignored", Path::new(".")) {
            Err(AdapterError::Flaked(reason)) => {
                assert!(reason.contains("code 3"), "reason: {reason}");
                assert!(reason.contains("boom-on-stderr"), "reason: {reason}");
            }
            other => panic!("expected Flaked, got {other:?}"),
        }
    }

    #[test]
    fn zero_exit_with_output_is_a_reply() {
        #[cfg(windows)]
        let a = shell("spectyn", "echo hi & exit /b 0");
        #[cfg(not(windows))]
        let a = shell("spectyn", "echo hi; exit 0");
        let r = a
            .run("ignored", Path::new("."))
            .expect("exit 0 + output is Ok");
        assert_eq!(r.text, "hi");
    }

    #[test]
    fn non_zero_exit_with_a_rate_limit_line_is_rate_limited_not_flaked() {
        #[cfg(windows)]
        let a = shell(
            "codex",
            "echo ERROR: You've hit your usage limit. Try again at Jun 25th, 2026 5:33 AM. & exit /b 1",
        );
        #[cfg(not(windows))]
        let a = shell(
            "codex",
            "echo \"ERROR: You've hit your usage limit. Try again at Jun 25th, 2026 5:33 AM.\"; exit 1",
        );
        match a.run("ignored", Path::new(".")) {
            Err(AdapterError::RateLimited(info)) => {
                assert_eq!(info.retry_at.as_deref(), Some("Jun 25th, 2026 5:33 AM"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn tail_chars_keeps_the_end_on_a_char_boundary() {
        assert_eq!(tail_chars("short", 300), "short");
        assert_eq!(tail_chars("abcdé", 2), "…dé");
    }

    #[test]
    fn external_abort_kills_a_running_turn_mid_flight() {
        // S1b `--hard`: a long turn that the operator hard-aborts must die near the abort, not run
        // to its generous timeout. Windows process-tree teardown can be noisy under CI load, so the
        // Windows bound proves we do not wait out the 30s adapter timeout instead of asserting an
        // unrealistically tight scheduler-dependent wall clock.
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
        let limit = Duration::from_secs(20);
        #[cfg(not(windows))]
        let limit = Duration::from_secs(3);
        assert!(
            start.elapsed() < limit,
            "must die near the abort: {:?}",
            start.elapsed()
        );
    }
}
