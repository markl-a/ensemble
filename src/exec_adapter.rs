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

/// How a vendor's stdout becomes the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplyFormat {
    /// The whole stdout, trimmed, is the answer (codex / claude / opencode).
    Text,
    /// `spectyn exec --json`: one JSON event per line; the `done` event carries the answer.
    SpectynJsonl,
}

/// Drives a vendor CLI headlessly by exec: `program <args...>` with the prompt on stdin, capturing
/// stdout as the reply. Per-vendor invocation contracts live in the constructors (design §4a).
pub struct ExecAdapter {
    name: String,
    program: String,
    args: Vec<String>,
    reply: ReplyFormat,
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
            reply: ReplyFormat::Text,
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
            reply: ReplyFormat::Text,
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
            reply: ReplyFormat::Text,
            timeout: Duration::from_secs(DEFAULT_EXEC_TIMEOUT_SECS),
            abort: Mutex::new(None),
        }
    }

    /// spectyn: `spectyn exec --json --agent coder` — reads the prompt on stdin and prints one JSON
    /// event per line; the reply is the `done` event's `output` (see `spectyn_reply`). Exit 0 ok /
    /// 1 agent error / 2 bad args. `ENSEMBLE_SPECTYN_BIN` overrides the binary.
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
            args: vec![
                "exec".into(),
                "--json".into(),
                "--agent".into(),
                "coder".into(),
            ],
            reply: ReplyFormat::SpectynJsonl,
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
        // spectyn --json: an answer spectyn delivered (exit 0, or the one excused early-stop exit 1)
        // is returned before the quota scan below, just as an exit-0 answer is never scanned: the
        // partial text may merely mention quota.
        let spectyn =
            (self.reply == ReplyFormat::SpectynJsonl).then(|| spectyn_reply(&text, status.code()));
        if let Some(SpectynReply::Answer(answer)) = spectyn {
            if answer.is_empty() {
                return Err(AdapterError::Empty);
            }
            return Ok(AgentOutput {
                agent: self.name.clone(),
                text: answer,
            });
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
        match spectyn {
            Some(SpectynReply::Error(msg)) => {
                return Err(AdapterError::Flaked(format!(
                    "{} reported an error: {}",
                    self.program,
                    tail_chars(msg.trim(), EXIT_TAIL_CHARS)
                )));
            }
            Some(SpectynReply::NoDone) if status.success() => {
                return Err(AdapterError::Flaked(format!(
                    "{} --json printed no done event",
                    self.program
                )));
            }
            // `ExitFailure` (only returned for a non-zero exit or a signal) or no `done` after a
            // failed exit: the generic non-zero handling below reports the code and stderr tail.
            _ => {}
        }
        // A non-zero exit is a failed turn even when the CLI printed something (spectyn exits 1 on an
        // agent error, codex on failures): that text is an error message, and counting it as a
        // reply could even turn it into a verdict. (spectyn's own early stops are handled above and
        // keep their text only with the visible "[spectyn stopped early: …]" note.)
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

/// The exact `error` lines `spectyn exec --json` prints after a `done` with `truncated: true`
/// (English, Chinese). Keep in sync with spectyn's `exec` (`exec_json_error_exit` callers).
const SPECTYN_TRUNCATED_LINES: [&str; 2] = [
    "error: the answer is INCOMPLETE — the stream ended before the model finished. The text above is a fragment; do not treat it as a complete answer.",
    "錯誤:這個答案不完整——串流在模型講完之前就結束了。上面的文字是片段,不要當成完整答案。",
];

/// ... and after a `done` that stopped at `max_rounds`.
const SPECTYN_MAX_ROUNDS_LINES: [&str; 2] = [
    "error: the run STOPPED at its round limit before the task was finished (see [stopped: …] above). Raise max_rounds for this agent or split the task.",
    "錯誤:回合數用完,任務還沒做完就停了(見上面的 [stopped: …])。請調高這個 agent 的 max_rounds,或把任務拆小。",
];

/// What a `spectyn exec --json` run amounts to (its stdout plus its exit code).
#[derive(Debug, PartialEq, Eq)]
enum SpectynReply {
    /// The reply: the last `done` event's `output`, with `[spectyn stopped early: <reason>]`
    /// appended when that event says the run did not finish (`truncated`, `stop_reason`, or the
    /// older `stopped`).
    Answer(String),
    /// A top-level `{"type":"error"}` line (its `message`), or an early stop with no text at all.
    Error(String),
    /// The exit code alone is the failure: non-zero (or a signal) with nothing that excuses it.
    ExitFailure,
    /// Neither a `done` nor an `error` event.
    NoDone,
}

/// Parse a `spectyn exec --json` run: one JSON event per line (non-JSON lines are ignored) and
/// the exit `code` (`None` = killed by a signal).
///
/// Any `error` event fails the turn, with ONE narrow exception. When the `done` event reports an
/// early stop, spectyn prints exactly one `error` line right after it explaining that stop and
/// exits 1 (e.g. "the answer is INCOMPLETE" for `truncated`, "… Raise max_rounds …" for
/// `max_rounds`). Only that line — the very next event, the last one, and saying the same thing as
/// the stop reason — together with exit code 1 is excused; the partial text is then returned with
/// the note so reviewers and the gate can see it was cut short. Any other error event, any other
/// non-zero exit code, or a signal stays a failure. (An older spectyn that reports the stop and
/// exits 0 with no error line also delivers the text with the note.)
fn spectyn_reply(stdout: &str, code: Option<i32>) -> SpectynReply {
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .collect();
    let kind = |ev: &serde_json::Value| ev.get("type").and_then(|t| t.as_str()).map(str::to_string);
    let message = |ev: &serde_json::Value| {
        ev.get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("error event {ev}"))
    };
    let errors: Vec<usize> = (0..events.len())
        .filter(|&i| kind(&events[i]).as_deref() == Some("error"))
        .collect();
    let Some(d) = (0..events.len())
        .rev()
        .find(|&i| kind(&events[i]).as_deref() == Some("done"))
    else {
        return errors.first().map_or(SpectynReply::NoDone, |&i| {
            SpectynReply::Error(message(&events[i]))
        });
    };
    let done = &events[d];
    let field = |k: &str| {
        done.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let output = done
        .get("output")
        .and_then(|o| o.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let truncated = done.get("truncated").and_then(|t| t.as_bool()) == Some(true);
    let stop_reason = field("stop_reason");
    let stopped = field("stopped");
    let Some(reason) = stop_reason
        .clone()
        .or_else(|| stopped.clone())
        .or_else(|| truncated.then(|| "truncated".to_string()))
    else {
        // The model finished: any error event or non-zero exit is a failure.
        return match (errors.first(), code) {
            (Some(&i), _) => SpectynReply::Error(message(&events[i])),
            (None, Some(0)) => SpectynReply::Answer(output),
            (None, _) => SpectynReply::ExitFailure,
        };
    };
    // Does `msg` explain THIS stop? Only the exact line spectyn prints for it (English or Chinese,
    // `exec_json_error_exit` in spectyn's `exec`), never a substring: a real failure that merely
    // contains "incomplete" must stay a failure.
    let max_rounds = [&stop_reason, &stopped]
        .into_iter()
        .flatten()
        .any(|r| r == "max_rounds");
    let explains = |msg: &str| {
        let msg = msg.trim();
        (truncated && SPECTYN_TRUNCATED_LINES.contains(&msg))
            || (max_rounds && SPECTYN_MAX_ROUNDS_LINES.contains(&msg))
    };
    // Only the line right after `done`, as the last event, that explains the stop is excused; any
    // other error event is the real failure. (At most one event can pass, so `explained` means
    // exactly that one line is present.)
    let excused = |e: usize| e == d + 1 && e + 1 == events.len() && explains(&message(&events[e]));
    // Report the real failure: an error that does not merely explain the stop, before a misplaced
    // explaining line.
    if let Some(&e) = errors
        .iter()
        .find(|&&e| !excused(e) && !explains(&message(&events[e])))
        .or_else(|| errors.iter().find(|&&e| !excused(e)))
    {
        return SpectynReply::Error(message(&events[e]));
    }
    let explained = !errors.is_empty();
    match (code, explained) {
        (Some(0), _) | (Some(1), true) => {}
        _ => return SpectynReply::ExitFailure,
    }
    if output.is_empty() {
        return SpectynReply::Error(format!("stopped early before any answer: {reason}"));
    }
    SpectynReply::Answer(format!("{output}\n\n[spectyn stopped early: {reason}]"))
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
            reply: ReplyFormat::Text,
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
        assert_eq!(a.args, vec!["exec", "--json", "--agent", "coder"]);
        assert_eq!(a.reply, ReplyFormat::SpectynJsonl);
        assert_eq!(ExecAdapter::codex().reply, ReplyFormat::Text);
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
        assert_eq!(args, ["exec", "--json", "--agent", "coder"]);
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

    const SPECTYN_TOOL_LINES: &str = concat!(
        r#"{"type":"tool_start","name":"file_read","args_preview":"{}"}"#,
        "\n",
        r#"{"type":"tool_done","name":"file_read","output_preview":"ok"}"#,
        "\n",
    );

    /// A top-level `error` event line carrying `msg`.
    fn error_line(msg: &str) -> String {
        serde_json::json!({"type": "error", "code": 1, "message": msg}).to_string()
    }

    /// The `done` + explaining `error` pair spectyn really prints for a cut-off stream.
    const SPECTYN_TRUNCATED: &str = concat!(
        r#"{"type":"done","output":"edit the li","files_changed":[],"truncated":true}"#,
        "\n",
        r#"{"type":"error","code":1,"message":"error: the answer is INCOMPLETE — the stream ended before the model finished. The text above is a fragment; do not treat it as a complete answer."}"#,
        "\n",
    );

    /// ... and for a run that used up its rounds (older builds: `stopped`, no `stop_reason`).
    const SPECTYN_MAX_ROUNDS: &str = concat!(
        r#"{"type":"done","output":"half done","truncated":false,"stopped":"max_rounds"}"#,
        "\n",
        r#"{"type":"error","code":1,"message":"error: the run STOPPED at its round limit before the task was finished (see [stopped: …] above). Raise max_rounds for this agent or split the task."}"#,
        "\n",
    );

    #[test]
    fn spectyn_reply_takes_the_done_output() {
        let out = format!(
            "{SPECTYN_TOOL_LINES}{}\n",
            r#"{"type":"done","output":"  VERDICT: APPROVE\nall good  ","cost_usd":0.0,"elapsed_secs":1.5,"files_changed":["src/a.rs"],"truncated":false}"#
        );
        assert_eq!(
            spectyn_reply(&out, Some(0)),
            SpectynReply::Answer("VERDICT: APPROVE\nall good".into())
        );
        // a clean done with a non-zero exit is still a failed turn (WP-100)
        assert_eq!(spectyn_reply(&out, Some(1)), SpectynReply::ExitFailure);
        assert_eq!(spectyn_reply(&out, None), SpectynReply::ExitFailure);
    }

    #[test]
    fn spectyn_reply_error_event_is_an_error() {
        let out = format!(
            "{SPECTYN_TOOL_LINES}{}\n",
            r#"{"type":"error","code":1,"message":"provider unreachable"}"#
        );
        assert_eq!(
            spectyn_reply(&out, Some(1)),
            SpectynReply::Error("provider unreachable".into())
        );
        // an error line after a clean done (e.g. every tool call was denied) still fails the turn
        let out = format!(
            "{}\n{}\n",
            r#"{"type":"done","output":"I could not edit anything","truncated":false}"#,
            r#"{"type":"error","code":1,"message":"all tools were denied"}"#
        );
        assert_eq!(
            spectyn_reply(&out, Some(1)),
            SpectynReply::Error("all tools were denied".into())
        );
    }

    #[test]
    fn spectyn_reply_without_done_is_no_done() {
        assert_eq!(
            spectyn_reply(SPECTYN_TOOL_LINES, Some(0)),
            SpectynReply::NoDone
        );
        assert_eq!(spectyn_reply("", Some(0)), SpectynReply::NoDone);
        assert_eq!(
            spectyn_reply("plain text, not json\n", Some(1)),
            SpectynReply::NoDone
        );
    }

    #[test]
    fn spectyn_reply_stopped_early_with_its_explaining_error_and_exit_1_keeps_the_text() {
        assert_eq!(
            spectyn_reply(SPECTYN_TRUNCATED, Some(1)),
            SpectynReply::Answer("edit the li\n\n[spectyn stopped early: truncated]".into())
        );
        assert_eq!(
            spectyn_reply(SPECTYN_MAX_ROUNDS, Some(1)),
            SpectynReply::Answer("half done\n\n[spectyn stopped early: max_rounds]".into())
        );
        // spectyn prints the same lines in Chinese under a zh locale
        let zh = format!(
            "{}\n{}",
            r#"{"type":"done","output":"edit the li","truncated":true}"#,
            error_line(SPECTYN_TRUNCATED_LINES[1])
        );
        assert_eq!(
            spectyn_reply(&zh, Some(1)),
            SpectynReply::Answer("edit the li\n\n[spectyn stopped early: truncated]".into())
        );
        // an older build that reports the stop and exits 0 with no error line
        let old = r#"{"type":"done","output":"edit the li","truncated":true}"#;
        assert_eq!(
            spectyn_reply(old, Some(0)),
            SpectynReply::Answer("edit the li\n\n[spectyn stopped early: truncated]".into())
        );
    }

    #[test]
    fn spectyn_reply_stop_reason_wins_over_stopped() {
        let out = format!(
            "{}\n{}",
            r#"{"type":"done","output":"half done","truncated":false,"stopped":"max_rounds","stop_reason":"round limit reached"}"#,
            error_line(SPECTYN_MAX_ROUNDS_LINES[0])
        );
        assert_eq!(
            spectyn_reply(&out, Some(1)),
            SpectynReply::Answer(
                "half done\n\n[spectyn stopped early: round limit reached]".into()
            )
        );
        // an early stop with no text at all has nothing to hand on
        let out = r#"{"type":"done","output":"","truncated":true}"#;
        assert!(
            matches!(spectyn_reply(out, Some(0)), SpectynReply::Error(m) if m.contains("truncated"))
        );
    }

    #[test]
    fn spectyn_reply_stopped_early_with_an_unrelated_error_is_an_error() {
        // the line after done says something else: that is the failure, not the stop
        let out = concat!(
            r#"{"type":"done","output":"edit the li","truncated":true}"#,
            "\n",
            r#"{"type":"error","code":1,"message":"provider unreachable"}"#,
        );
        assert_eq!(
            spectyn_reply(out, Some(1)),
            SpectynReply::Error("provider unreachable".into())
        );
        // the right line, but another error came before done
        let out = format!(
            "{}\n{SPECTYN_TRUNCATED}",
            r#"{"type":"error","code":1,"message":"disk full"}"#
        );
        assert_eq!(
            spectyn_reply(&out, Some(1)),
            SpectynReply::Error("disk full".into())
        );
        // the right line, but not the last event (something failed after it)
        let out = format!(
            "{SPECTYN_TRUNCATED}{}\n",
            r#"{"type":"error","code":1,"message":"session store write failed"}"#
        );
        assert_eq!(
            spectyn_reply(&out, Some(1)),
            SpectynReply::Error("session store write failed".into())
        );
        // a real failure that merely contains the stop's words is not the explaining line
        for msg in [
            "permission denied; incomplete output",
            "error: the answer is INCOMPLETE",
            "provider error; raise max_rounds? no: connection reset",
        ] {
            let out = format!(
                "{}\n{}",
                r#"{"type":"done","output":"edit the li","truncated":true,"stopped":"max_rounds"}"#,
                error_line(msg)
            );
            assert_eq!(
                spectyn_reply(&out, Some(1)),
                SpectynReply::Error(msg.into()),
                "{msg}"
            );
        }
        // the max_rounds wording does not excuse a truncated stop
        let out = concat!(
            r#"{"type":"done","output":"edit the li","truncated":true}"#,
            "\n",
            r#"{"type":"error","code":1,"message":"error: the run STOPPED at its round limit."}"#,
        );
        assert!(matches!(
            spectyn_reply(out, Some(1)),
            SpectynReply::Error(_)
        ));
    }

    #[test]
    fn spectyn_reply_stopped_early_with_another_exit_code_or_a_signal_is_an_exit_failure() {
        for code in [Some(2), Some(101), Some(-1), None] {
            assert_eq!(
                spectyn_reply(SPECTYN_TRUNCATED, code),
                SpectynReply::ExitFailure,
                "code {code:?}"
            );
            assert_eq!(
                spectyn_reply(SPECTYN_MAX_ROUNDS, code),
                SpectynReply::ExitFailure,
                "code {code:?}"
            );
        }
        // exit 1 with no explaining line: something else failed
        let out = r#"{"type":"done","output":"edit the li","truncated":true}"#;
        assert_eq!(spectyn_reply(out, Some(1)), SpectynReply::ExitFailure);
    }

    /// A fake `spectyn exec --json` that prints `jsonl` and exits with `code`.
    fn fake_spectyn(tag: &str, jsonl: &str, code: i32) -> (ExecAdapter, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("ensemble-wp172-{tag}-{}.jsonl", std::process::id()));
        std::fs::write(&path, jsonl).unwrap();
        #[cfg(windows)]
        let script = format!("type {} & exit /b {code}", path.display()); // no quotes: cmd /C mangles them
        #[cfg(not(windows))]
        let script = format!("cat '{}'; exit {code}", path.display());
        let mut a = shell("spectyn", &script);
        a.reply = ReplyFormat::SpectynJsonl;
        (a, path)
    }

    #[test]
    fn spectyn_run_returns_done_output_and_flakes_on_error_or_missing_done() {
        let (a, p) = fake_spectyn(
            "ok",
            "{\"type\":\"tool_start\",\"name\":\"ls\",\"args_preview\":\"{}\"}\n{\"type\":\"done\",\"output\":\"hello\",\"truncated\":false}\n",
            0,
        );
        let r = a.run("ignored", Path::new("."));
        let _ = std::fs::remove_file(&p);
        assert_eq!(r.expect("done output is the reply").text, "hello");

        let (a, p) = fake_spectyn(
            "err",
            "{\"type\":\"error\",\"code\":1,\"message\":\"model refused\"}\n",
            1,
        );
        let r = a.run("ignored", Path::new("."));
        let _ = std::fs::remove_file(&p);
        match r {
            Err(AdapterError::Flaked(m)) => assert!(m.contains("model refused"), "reason: {m}"),
            other => panic!("expected Flaked, got {other:?}"),
        }

        let (a, p) = fake_spectyn("nodone", "{\"type\":\"token\",\"content\":\"hi\"}\n", 0);
        let r = a.run("ignored", Path::new("."));
        let _ = std::fs::remove_file(&p);
        match r {
            Err(AdapterError::Flaked(m)) => assert!(m.contains("no done event"), "reason: {m}"),
            other => panic!("expected Flaked, got {other:?}"),
        }
    }

    #[test]
    fn spectyn_run_keeps_a_truncated_answer_only_with_exit_1() {
        let (a, p) = fake_spectyn("trunc", SPECTYN_TRUNCATED, 1);
        let r = a.run("ignored", Path::new("."));
        let _ = std::fs::remove_file(&p);
        let text = r.expect("a cut-off answer is still handed on").text;
        assert!(text.starts_with("edit the li"), "{text}");
        assert!(
            text.ends_with("[spectyn stopped early: truncated]"),
            "{text}"
        );

        // a delivered partial answer that mentions quota is not mistaken for a rate limit
        let quota = format!(
            "{}\n{}\n",
            r#"{"type":"done","output":"fix the quota check, then the rate limit te","truncated":true}"#,
            error_line(SPECTYN_TRUNCATED_LINES[0])
        );
        let (a, p) = fake_spectyn("quota", &quota, 1);
        let r = a.run("ignored", Path::new("."));
        let _ = std::fs::remove_file(&p);
        let text = r.expect("an excused partial answer is a reply").text;
        assert!(text.starts_with("fix the quota check"), "{text}");

        for code in [2, 101] {
            let (a, p) = fake_spectyn(&format!("trunc{code}"), SPECTYN_TRUNCATED, code);
            let r = a.run("ignored", Path::new("."));
            let _ = std::fs::remove_file(&p);
            match r {
                Err(AdapterError::Flaked(m)) => {
                    assert!(m.contains(&format!("code {code}")), "reason: {m}")
                }
                other => panic!("exit {code} must fail, got {other:?}"),
            }
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
