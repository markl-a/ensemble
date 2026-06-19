# ensemble Phase-1b — real vendor adapters (claude, opencode, agy)

> REQUIRED SUB-SKILL: subagent-driven-development. TDD. **Build/test via WSL** (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`) — native Windows hits LNK1104 (Defender locks target .exe). Work in `D:\Projects\ensemble` on `main`.

**Goal:** `ensemble run` drives all four real CLIs. claude + opencode are trivial exec adapters; **agy needs a real PTY** because `agy -p` emits zero bytes to a non-TTY stdout (upstream antigravity-cli#76). Hermetic-test what we can (ANSI strip, adapter wiring); live CLI round-trips are `#[ignore]` smokes.

**Architecture:** Reuse the existing `ExecAdapter` for claude/opencode. Add a new PTY-based `AgyAdapter` (portable-pty) with a wall-clock timeout + ANSI strip + empty-detection (mirrors the proven agy_pty.py hardening). Register all four in `main.rs`.

---

### Task 1: claude + opencode exec adapters

**Files:** Modify `src/exec_adapter.rs`.

- [ ] **Step 1 (test):** add to `exec_adapter.rs` `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_programs() {
        assert_eq!(ExecAdapter::codex().name(), "codex");
        assert_eq!(ExecAdapter::claude().name(), "claude");
        assert_eq!(ExecAdapter::opencode().name(), "opencode");
    }
}
```

- [ ] **Step 2:** run `cargo test --lib exec_adapter` → FAIL (claude/opencode undefined).

- [ ] **Step 3 (impl):** add two constructors to `impl ExecAdapter`:

```rust
/// claude: `claude -p <prompt>` — prints the answer to stdout (headless).
pub fn claude() -> Self {
    Self { name: "claude".into(), program: "claude".into(), args: vec!["-p".into()] }
}
/// opencode: `opencode run <prompt>`.
pub fn opencode() -> Self {
    Self { name: "opencode".into(), program: "opencode".into(), args: vec!["run".into()] }
}
```

- [ ] **Step 4:** `cargo test --lib exec_adapter` → PASS.
- [ ] **Step 5:** commit `feat(phase1b): claude + opencode exec adapters`.

---

### Task 2: `AgyAdapter` (PTY) + hermetic `strip_ansi`

**Files:** Modify `Cargo.toml` (add `portable-pty = "0.8"`); Create `src/agy_adapter.rs`; Modify `src/lib.rs` (`pub mod agy_adapter; pub use agy_adapter::AgyAdapter;`).

- [ ] **Step 1 (test — the hermetic part):** in `src/agy_adapter.rs` test module:

```rust
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
}
```

- [ ] **Step 2:** `cargo test --lib agy_adapter` → FAIL.

- [ ] **Step 3 (impl):** `src/agy_adapter.rs`:

```rust
use crate::adapter::{Adapter, AdapterError, AgentOutput};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// Drives Google Antigravity (`agy`) headlessly UNDER A REAL PTY. `agy -p` emits zero bytes to a
/// non-TTY stdout (upstream antigravity-cli#76), so a plain exec yields Empty; a PTY makes the
/// answer appear. Output is ANSI-stripped; a stalled agy is killed at `timeout` and reported as
/// Flaked (never hung). Mirrors the proven agy_pty.py approach.
pub struct AgyAdapter {
    timeout: Duration,
}

impl AgyAdapter {
    pub fn new() -> Self { Self { timeout: Duration::from_secs(180) } }
    pub fn with_timeout(timeout: Duration) -> Self { Self { timeout } }
}

impl Default for AgyAdapter {
    fn default() -> Self { Self::new() }
}

impl Adapter for AgyAdapter {
    fn name(&self) -> &str { "agy" }

    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: 40, cols: 200, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| AdapterError::Flaked(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new("agy");
        cmd.arg("-p");
        cmd.arg(prompt);
        cmd.cwd(cwd);

        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("not found") || msg.contains("no such file") || msg.contains("cannot find") {
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

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,            // EOF: child exited
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let _ = tx.send(buf);
        });

        let raw = match rx.recv_timeout(self.timeout) {
            Ok(buf) => buf,
            Err(_) => {
                let _ = child.kill();
                return Err(AdapterError::Flaked(format!("agy timed out after {:?}", self.timeout)));
            }
        };
        let _ = child.wait();
        drop(pair.master);

        let text = strip_ansi(&String::from_utf8_lossy(&raw)).trim().to_string();
        if text.is_empty() {
            return Err(AdapterError::Empty);
        }
        Ok(AgentOutput { agent: "agy".into(), text })
    }
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
                while i < bytes.len() && !('@'..='~').contains(&bytes[i]) { i += 1; }
                i += 1; // consume the final byte
                continue;
            }
            if i + 1 < bytes.len() && bytes[i + 1] == ']' {
                i += 2;
                while i < bytes.len() && bytes[i] != '\x07' && bytes[i] != '\x1b' { i += 1; }
                if i < bytes.len() && bytes[i] == '\x1b' { i += 1; } // ST: ESC \
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
```

- [ ] **Step 4:** `cargo test --lib agy_adapter` → PASS (the 2 hermetic tests). `cargo build` compiles.
- [ ] **Step 5:** commit `feat(phase1b): AgyAdapter (PTY) + hermetic strip_ansi`.

---

### Task 3: register all four adapters in the CLI + live smokes

**Files:** Modify `src/main.rs`; Modify `tests/live_smoke.rs`.

- [ ] **Step 1:** in `src/main.rs`, replace the codex-only registry with all four:

```rust
let mut adapters: HashMap<String, Box<dyn Adapter>> = HashMap::new();
adapters.insert("codex".into(), Box::new(ExecAdapter::codex()));
adapters.insert("claude".into(), Box::new(ExecAdapter::claude()));
adapters.insert("opencode".into(), Box::new(ExecAdapter::opencode()));
adapters.insert("agy".into(), Box::new(AgyAdapter::new()));
```

(Ensure `use ensemble::*;` re-exports `AgyAdapter` — add to lib.rs in Task 2.)

- [ ] **Step 2:** add `#[ignore]` live smokes to `tests/live_smoke.rs` for claude, opencode, agy (mirror the codex one — each asserts a PONG round-trip but tolerates `NotInstalled`):

```rust
#[test]
#[ignore = "live: requires `agy` CLI on PATH + interactive auth"]
fn agy_pty_adapter_answers() {
    let a = AgyAdapter::new();
    match a.run("Reply with exactly one word: PONG", std::path::Path::new(".")) {
        Ok(out) => { assert_eq!(out.agent, "agy"); assert!(out.text.to_uppercase().contains("PONG")); }
        Err(AdapterError::NotInstalled(_)) => eprintln!("agy not installed — skipping"),
        Err(e) => panic!("agy live smoke failed: {e}"),
    }
}
// + claude_exec_adapter_answers / opencode_exec_adapter_answers (same shape, ExecAdapter::claude()/opencode()).
```

- [ ] **Step 3:** `cargo test` (all hermetic green; live smokes ignored), `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`.
- [ ] **Step 4:** commit `feat(phase1b): register codex/claude/opencode/agy in CLI + live smokes`.

---

## Notes / deferred
- agy `--output-format json` was rejected on agy 1.0.10 (prior finding) → Phase 1b ships PTY-only; if a future agy adds clean JSON, add a probe path.
- The PTY adapter captures on child-exit/timeout; per-line streaming + the protobuf-.db transcript fallback are deferred.
- Real git-worktree isolation + parallel pipelines + Retry/Substitute on_flake = **Phase 2**.
