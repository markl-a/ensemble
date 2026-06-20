use crate::adapter::{Adapter, AdapterError, AgentOutput};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Drives Google Antigravity (`agy`) headlessly UNDER A REAL PTY. `agy -p` emits zero bytes to a
/// non-TTY stdout (upstream antigravity-cli#76), so a plain exec yields Empty; a PTY makes the
/// answer appear. Output is ANSI-stripped; a stalled agy is killed at `timeout` and reported as
/// Flaked (never hung). Mirrors the proven agy_pty.py approach.
pub struct AgyAdapter {
    timeout: Duration,
}

impl AgyAdapter {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(180),
        }
    }
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for AgyAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for AgyAdapter {
    fn name(&self) -> &str {
        "agy"
    }

    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 40,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AdapterError::Flaked(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new("agy");
        for a in agy_argv(prompt, self.timeout) {
            cmd.arg(a);
        }
        cmd.cwd(cwd);

        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("not found")
                    || msg.contains("no such file")
                    || msg.contains("cannot find")
                {
                    return Err(AdapterError::NotInstalled("agy".into()));
                }
                return Err(AdapterError::Flaked(format!("spawn agy: {e}")));
            }
        };
        drop(pair.slave); // parent keeps only the master end

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AdapterError::Flaked(format!("clone reader: {e}")))?;

        // Drain the PTY into a SHARED buffer on a worker thread; it signals `done` when its read loop
        // ends (EOF/err). The lock is poison-safe so a panicking holder can't crash the adapter.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let buf_w = Arc::clone(&buf);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf_w
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let _ = done_tx.send(());
        });

        // Wait on the CHILD's EXIT, not the master's EOF: portable-pty's Windows ConPTY master can
        // stay open after agy exits, so an EOF-only wait hung to the full timeout. Poll is-alive,
        // reaping on every exit/kill so we never leave a zombie.
        let deadline = Instant::now() + self.timeout;
        let mut fail: Option<String> = None;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break, // agy exited cleanly (try_wait reaps it)
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    fail = Some(format!("agy timed out after {:?}", self.timeout));
                    break;
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    fail = Some(format!("agy wait failed: {e}"));
                    break;
                }
            }
        }
        // Close the master to force the reader to EOF, then wait (bounded) for it to finish draining —
        // a deterministic FULL capture (no magic sleep, no leaked/blocked thread). On a clean Unix
        // exit the reader already hit EOF, so `done` is immediate; on Windows the drop releases it.
        drop(pair.master);
        let _ = done_rx.recv_timeout(Duration::from_secs(2));

        if let Some(reason) = fail {
            // A genuine hang/error: do NOT trust a partial answer from a killed model call.
            return Err(AdapterError::Flaked(reason));
        }
        let raw = buf.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let text = strip_ansi(&String::from_utf8_lossy(&raw)).trim().to_string();
        if text.is_empty() {
            return Err(AdapterError::Empty);
        }
        Ok(AgentOutput {
            agent: "agy".into(),
            text,
        })
    }
}

/// agy's argv (without the leading "agy"): `--print-timeout <N>s` BEFORE `-p <prompt>`. The
/// `--print-timeout` is the proven agy_pty.py fix — without it, agy under a PTY can take the
/// MCP-init / cold-auth code path and STALL forever (antigravity-cli#76), tripping our wall-clock
/// kill as a flake. `N` = our timeout minus ~15s (min 10s) so agy self-terminates its model call
/// just before we would hard-kill it.
fn agy_argv(prompt: &str, timeout: Duration) -> Vec<String> {
    let t = timeout.as_secs();
    // print-timeout must stay STRICTLY BELOW our wall-clock kill so agy self-terminates its model
    // call first; the `.min(t-2)` cap preserves that invariant even for small timeouts.
    let print_to = t.saturating_sub(15).max(10).min(t.saturating_sub(2));
    vec![
        "--print-timeout".into(),
        format!("{print_to}s"),
        "-p".into(),
        prompt.into(),
    ]
}

/// Strip ANSI/OSC escape sequences and bare control chars (keep \n \t) — agy's PTY output is a
/// TUI stream. Hand-rolled (no regex dep).
pub(crate) fn strip_ansi(s: &str) -> String {
    let bytes: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '\x1b' {
            // CSI: ESC '[' ... final byte in @..~ ; OSC: ESC ']' ... BEL or ESC '\'
            if i + 1 < bytes.len() && bytes[i + 1] == '[' {
                i += 2;
                while i < bytes.len() && !('@'..='~').contains(&bytes[i]) {
                    i += 1;
                }
                i += 1; // consume the final byte
                continue;
            }
            if i + 1 < bytes.len() && bytes[i + 1] == ']' {
                i += 2;
                while i < bytes.len() && bytes[i] != '\x07' && bytes[i] != '\x1b' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == '\x1b' {
                    i += 1;
                } // ST: ESC \
                i += 1; // consume BEL or the '\'
                continue;
            }
            i += 1; // lone ESC
            continue;
        }
        if (c as u32) < 0x20 && c != '\n' && c != '\t' {
            i += 1; // drop other control chars (incl \r)
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_ansi_and_control() {
        let raw = "\x1b[2J\x1b[33mPONG\x1b[0m\r\n\x1b]0;title\x07done";
        let cleaned = strip_ansi(raw);
        assert!(cleaned.contains("PONG"));
        assert!(cleaned.contains("done"));
        assert!(!cleaned.contains('\x1b'));
    }
    #[test]
    fn adapter_name_is_agy() {
        assert_eq!(AgyAdapter::new().name(), "agy");
    }

    #[test]
    fn agy_argv_puts_print_timeout_before_p() {
        // the agy_pty.py fix: --print-timeout <N>s must precede -p so agy self-terminates first.
        let argv = agy_argv("say PONG", Duration::from_secs(180));
        assert_eq!(argv, vec!["--print-timeout", "165s", "-p", "say PONG"]);
    }

    #[test]
    fn agy_argv_print_timeout_stays_below_wall_clock() {
        // agy must self-terminate BEFORE our hard kill — print-timeout < wall-clock for any value.
        for secs in [5u64, 20, 180] {
            let argv = agy_argv("x", Duration::from_secs(secs));
            let pt: u64 = argv[1].trim_end_matches('s').parse().unwrap();
            assert!(pt < secs, "print_to {pt}s must be < wall-clock {secs}s");
        }
    }
}
