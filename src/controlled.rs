use crate::supervise::{member_control_path, ControlCmd};
use crate::Feed;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_PROMPT_DELAY: Duration = Duration::from_millis(250);
const CONTROL_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyProgram {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ControlledPtyConfig {
    pub label: String,
    pub repo: PathBuf,
    pub control_name: String,
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    pub envs: Vec<(OsString, OsString)>,
    pub prompt_delay: Duration,
    /// `None`: a steer's text and its Enter go out in ONE write, which is what
    /// codex/claude/opencode/agy expect. `Some(d)`: the text, then `d` of
    /// silence, then the Enter as a SEPARATE write — for a TUI with no bracketed
    /// paste, which treats keys arriving in one burst as a paste, so an Enter in
    /// the same burst becomes a newline in the input box instead of a submit.
    pub submit_delay: Option<Duration>,
    pub relay_output: bool,
}

impl ControlledPtyConfig {
    pub fn new(
        label: impl Into<String>,
        repo: impl Into<PathBuf>,
        control_name: impl Into<String>,
        cwd: impl Into<PathBuf>,
        program: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            label: label.into(),
            repo: repo.into(),
            control_name: control_name.into(),
            cwd: cwd.into(),
            program: program.into(),
            args,
            envs: Vec::new(),
            prompt_delay: DEFAULT_PROMPT_DELAY,
            submit_delay: None,
            relay_output: true,
        }
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    /// Send a steer's Enter as its own write, `delay` after the text. See
    /// `submit_delay`.
    pub fn submit_separately(mut self, delay: Duration) -> Self {
        self.submit_delay = Some(delay);
        self
    }

    pub fn relay_output(mut self, relay_output: bool) -> Self {
        self.relay_output = relay_output;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlScript {
    pub interrupt: bool,
    pub prompt: Option<String>,
    pub kill: bool,
}

pub fn pty_program_for_vendor(program: &str, args: &[String]) -> PtyProgram {
    #[cfg(windows)]
    {
        let mut shell_args = vec!["/C".to_string(), program.to_string()];
        shell_args.extend(args.iter().cloned());
        PtyProgram {
            program: "cmd".to_string(),
            args: shell_args,
        }
    }
    #[cfg(not(windows))]
    {
        PtyProgram {
            program: program.to_string(),
            args: args.to_vec(),
        }
    }
}

pub fn control_script(cmd: &ControlCmd) -> ControlScript {
    match cmd {
        ControlCmd::Steer {
            prompt, interrupt, ..
        } => ControlScript {
            interrupt: *interrupt,
            prompt: Some(prompt.clone()),
            kill: false,
        },
        ControlCmd::Abort { hard, .. } => ControlScript {
            interrupt: true,
            prompt: None,
            kill: *hard,
        },
    }
}

pub fn prompt_enter_bytes(prompt: &str) -> Vec<u8> {
    let mut bytes = prompt.as_bytes().to_vec();
    bytes.push(b'\r');
    bytes
}

fn write_script<W: Write>(
    writer: &Mutex<W>,
    script: &ControlScript,
    delay: Duration,
    submit_delay: Option<Duration>,
) {
    if script.interrupt {
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(b"\x1b");
            let _ = w.flush();
        }
    }
    if let Some(prompt) = &script.prompt {
        std::thread::sleep(delay);
        match submit_delay {
            None => {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(&prompt_enter_bytes(prompt));
                    let _ = w.flush();
                }
            }
            Some(gap) => {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(prompt.as_bytes());
                    let _ = w.flush();
                }
                // The lock is released during the gap so the operator's own
                // keystrokes are not stalled behind it.
                std::thread::sleep(gap);
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(b"\r");
                    let _ = w.flush();
                }
            }
        }
    }
}

/// The size of the terminal ensemble itself is running in.
///
/// This used to be hardcoded to 40×200. A vendor TUI lays itself out for the
/// size the PTY reports, so on any window that is not exactly 200 columns the
/// agent draws 200-column lines into a narrower terminal and every box, rule
/// and status line wraps into garbage. Falls back to the old constant when
/// there is no terminal to ask (piped output, headless, a CI runner).
fn host_pty_size() -> PtySize {
    match terminal_size::terminal_size() {
        Some((terminal_size::Width(cols), terminal_size::Height(rows))) if cols > 0 && rows > 0 => {
            PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            }
        }
        _ => PtySize {
            rows: 40,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        },
    }
}

pub fn run_controlled_pty(config: ControlledPtyConfig) -> Result<i32, String> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(host_pty_size())
        .map_err(|e| format!("openpty: {e}"))?;

    let program = pty_program_for_vendor(&config.program, &config.args);
    let mut cmd = CommandBuilder::new(&program.program);
    for arg in &program.args {
        cmd.arg(arg);
    }
    cmd.cwd(&config.cwd);

    // Tell the child it has a colour terminal. A ConPTY carries the escape
    // codes fine, but a CLI decides whether to EMIT them by looking at the
    // environment, and ensemble was passing only its own ENSEMBLE_* vars — so
    // agents came up monochrome under the launcher while the same CLI run
    // directly was in colour. For a coding agent that is not decoration:
    // diffs, error levels and approval prompts are colour-coded.
    //
    // Defaults, not overrides. An operator who has deliberately set TERM (or
    // NO_COLOR, which the vendors read themselves) keeps their choice, and
    // config.envs is applied afterwards so a caller can still win.
    if std::env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    if std::env::var_os("COLORTERM").is_none() {
        cmd.env("COLORTERM", "truecolor");
    }

    for (key, value) in &config.envs {
        cmd.env(key, value);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("start `{}`: {e}", config.program))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone pty reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take pty writer: {e}"))?;
    let writer = Arc::new(Mutex::new(writer));

    let (output_done_tx, output_done_rx) = mpsc::channel();
    let relay_output = config.relay_output;
    let output_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if relay_output {
                        let mut stdout = std::io::stdout().lock();
                        if stdout.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = stdout.flush();
                    }
                }
                Err(_) => break,
            }
        }
        let _ = output_done_tx.send(());
    });

    let input_writer = Arc::clone(&writer);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let Ok(mut w) = input_writer.lock() else {
                        break;
                    };
                    if w.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = w.flush();
                }
                Err(_) => break,
            }
        }
    });

    let hard_abort = Arc::new(AtomicBool::new(false));
    let control_abort = Arc::clone(&hard_abort);
    let control_writer = Arc::clone(&writer);
    let feed = Feed::open(member_control_path(&config.repo, &config.control_name));
    let mut cursor = feed.read_since(0).map(|lines| lines.len()).unwrap_or(0);
    let prompt_delay = config.prompt_delay;
    let submit_delay = config.submit_delay;
    std::thread::spawn(move || loop {
        std::thread::sleep(CONTROL_POLL);
        let lines = match feed.read_since(cursor) {
            Ok(lines) => lines,
            Err(_) => continue,
        };
        for line in &lines {
            let Ok(cmd) = serde_json::from_str::<ControlCmd>(line) else {
                continue;
            };
            let script = control_script(&cmd);
            write_script(&*control_writer, &script, prompt_delay, submit_delay);
            if script.kill {
                control_abort.store(true, Ordering::Relaxed);
            }
        }
        cursor += lines.len();
    });

    // Keep the PTY the same size as the window it is being watched in. Sizing
    // it once at startup is not enough: the operator drags the window, the
    // vendor TUI is never told, and it keeps laying out at the old width — the
    // same garbled output the hardcoded 40x200 produced, just triggered later.
    //
    // Windows has no SIGWINCH, so this polls, on the wait loop that was already
    // waking every 50 ms. Throttled to ~500 ms and only issued when the value
    // actually changed, so the child sees one resize per real change rather
    // than a stream of no-ops.
    let mut last_size = host_pty_size();
    let mut ticks: u32 = 0;

    let exit = loop {
        if hard_abort.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break 130;
        }
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(10) {
            let now = host_pty_size();
            if now.rows != last_size.rows || now.cols != last_size.cols {
                let _ = pair.master.resize(now);
                last_size = now;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.exit_code() as i32,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait `{}`: {e}", config.program));
            }
        }
    };

    drop(pair.master);
    if output_done_rx.recv_timeout(Duration::from_secs(2)).is_ok() {
        let _ = output_thread.join();
    }
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_program_uses_cmd_on_windows_for_ps1_cmd_shims() {
        let args = vec!["--continue".to_string()];
        let p = pty_program_for_vendor("claude", &args);
        #[cfg(windows)]
        assert_eq!(
            p,
            PtyProgram {
                program: "cmd".to_string(),
                args: vec![
                    "/C".to_string(),
                    "claude".to_string(),
                    "--continue".to_string()
                ]
            }
        );
        #[cfg(not(windows))]
        assert_eq!(
            p,
            PtyProgram {
                program: "claude".to_string(),
                args
            }
        );
    }

    #[test]
    fn steer_control_interrupts_and_injects_prompt_without_killing() {
        let script = control_script(&ControlCmd::Steer {
            from: "codex@host".to_string(),
            prompt: "focus on auth only".to_string(),
            interrupt: true,
        });
        assert_eq!(
            script,
            ControlScript {
                interrupt: true,
                prompt: Some("focus on auth only".to_string()),
                kill: false
            }
        );
        assert_eq!(prompt_enter_bytes("focus"), b"focus\r");
    }

    #[test]
    fn host_pty_size_is_never_the_old_hardcoded_guess_when_a_terminal_answers() {
        // The bug this replaces: every controlled session opened its PTY at
        // 40x200 regardless of the window, so a vendor TUI laid out 200-column
        // lines into whatever width the operator actually had and every box and
        // rule wrapped into garbage.
        //
        // Under `cargo test` stdout is captured, so `terminal_size` reports
        // nothing and this exercises the FALLBACK arm — which must still be a
        // usable size rather than zero, because a PTY sized 0x0 renders nothing
        // at all and would turn a cosmetic bug into a blank screen.
        let size = host_pty_size();

        assert!(
            size.rows > 0 && size.cols > 0,
            "unusable PTY size: {size:?}"
        );
        match terminal_size::terminal_size() {
            Some((terminal_size::Width(c), terminal_size::Height(r))) if c > 0 && r > 0 => {
                assert_eq!(
                    (size.cols, size.rows),
                    (c, r),
                    "a real terminal was available and was ignored",
                );
            }
            _ => {
                assert_eq!((size.cols, size.rows), (200, 40), "fallback changed");
            }
        }
    }

    #[test]
    fn a_steer_can_be_sent_without_the_interrupt_that_dismisses_modals() {
        // Concrete incident: agy parks on "Do you trust the contents of this
        // project?" at first launch. `steer` sent ESC before the prompt, the
        // dialog read that as cancel, and cancel there is "No, exit" — so the
        // agent died on every steer and it looked like agy could not start.
        //
        // ESC is still right for redirecting a turn already in flight, so this
        // is a choice per steer, not a change of default.
        let quiet = control_script(&ControlCmd::Steer {
            from: "operator".to_string(),
            prompt: "yes".to_string(),
            interrupt: false,
        });
        assert_eq!(
            quiet,
            ControlScript {
                interrupt: false,
                prompt: Some("yes".to_string()),
                kill: false,
            }
        );

        // Default unchanged.
        let loud = control_script(&ControlCmd::Steer {
            from: "operator".to_string(),
            prompt: "yes".to_string(),
            interrupt: true,
        });
        assert!(loud.interrupt);
    }

    #[test]
    fn a_control_line_written_before_the_interrupt_field_still_interrupts() {
        // Forward-compat on the wire: feeds written by an older binary have no
        // `interrupt` key, and must keep their old meaning rather than silently
        // becoming quiet steers.
        let old = r#"{"cmd":"steer","from":"operator","prompt":"focus"}"#;
        let cmd: ControlCmd = serde_json::from_str(old).expect("old line must still parse");

        assert!(
            control_script(&cmd).interrupt,
            "old steer stopped interrupting"
        );
    }

    /// Records every `write` call as its own chunk, with when it arrived, so a
    /// test can see how the keys were split across writes — the thing a
    /// no-bracketed-paste TUI actually reacts to.
    #[derive(Default)]
    struct ChunkLog {
        chunks: Vec<(std::time::Instant, Vec<u8>)>,
    }

    impl Write for ChunkLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.chunks.push((std::time::Instant::now(), buf.to_vec()));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn steer(prompt: &str) -> ControlScript {
        control_script(&ControlCmd::Steer {
            from: "operator".to_string(),
            prompt: prompt.to_string(),
            interrupt: true,
        })
    }

    #[test]
    fn a_steer_sends_text_and_enter_in_one_write_by_default() {
        // codex/claude/opencode/agy: unchanged — ESC, then text+CR together.
        let log = Mutex::new(ChunkLog::default());

        write_script(&log, &steer("focus"), Duration::ZERO, None);

        let chunks: Vec<Vec<u8>> = log
            .into_inner()
            .unwrap()
            .chunks
            .into_iter()
            .map(|c| c.1)
            .collect();
        assert_eq!(chunks, vec![b"\x1b".to_vec(), b"focus\r".to_vec()]);
    }

    #[test]
    fn a_separate_submit_sends_enter_as_its_own_write_after_the_gap() {
        // spectyn has no bracketed paste: text+CR in one burst reads as a paste
        // and the CR becomes a newline in its input box, so the steer is typed
        // but never submitted. The Enter has to arrive alone, after a pause.
        let gap = Duration::from_millis(40);
        let log = Mutex::new(ChunkLog::default());

        write_script(&log, &steer("focus"), Duration::ZERO, Some(gap));

        let chunks = log.into_inner().unwrap().chunks;
        let bytes: Vec<&[u8]> = chunks.iter().map(|c| c.1.as_slice()).collect();
        assert_eq!(bytes, vec![&b"\x1b"[..], &b"focus"[..], &b"\r"[..]]);
        assert!(
            chunks[2].0.duration_since(chunks[1].0) >= gap,
            "Enter followed the text with no pause"
        );
    }

    #[test]
    fn a_separate_submit_is_set_by_the_builder_and_off_by_default() {
        let base = ControlledPtyConfig::new("t", ".", "t@local", ".", "x", Vec::new());
        assert_eq!(base.submit_delay, None);
        let split = base.submit_separately(Duration::from_millis(350));
        assert_eq!(split.submit_delay, Some(Duration::from_millis(350)));
    }

    #[test]
    fn hard_abort_interrupts_and_kills() {
        let script = control_script(&ControlCmd::Abort {
            from: "operator".to_string(),
            hard: true,
        });
        assert_eq!(
            script,
            ControlScript {
                interrupt: true,
                prompt: None,
                kill: true
            }
        );
    }

    #[test]
    fn controlled_pty_returns_child_exit_code() {
        #[cfg(windows)]
        let config = ControlledPtyConfig::new(
            "test",
            std::env::current_dir().unwrap(),
            "test@local",
            std::env::current_dir().unwrap(),
            "cmd",
            vec![
                "/C".to_string(),
                "exit".to_string(),
                "/b".to_string(),
                "7".to_string(),
            ],
        )
        .relay_output(false);
        #[cfg(not(windows))]
        let config = ControlledPtyConfig::new(
            "test",
            std::env::current_dir().unwrap(),
            "test@local",
            std::env::current_dir().unwrap(),
            "sh",
            vec!["-c".to_string(), "exit 7".to_string()],
        )
        .relay_output(false);

        let code = run_controlled_pty(config).unwrap();

        assert_eq!(code, 7);
    }
}
