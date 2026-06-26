# S1b — Supervision control plane (steer / abort) Implementation Plan

> **For agentic workers:** TDD task-by-task. Pure logic in lib (TDD'd); IO shells in main.rs are
> gate-reviewed + smoke-tested. Steps use `- [ ]` checkboxes.

**Goal:** Let the operator actively STEER and INTERRUPT a live governed `ensemble run` to keep an AI CLI
from going off-track — `ensemble steer <name> "<prompt>"` injects a redirect into the next round, and
`ensemble abort <name> [--hard]` stops it (clean at the round boundary; `--hard` kill_tree's the
currently-running CLI immediately). File-plane, cross-platform, zero Windows-ConPTY dependence.

**Architecture:** Reuse the S1a observe wiring. The SAME `<name>` that `ensemble run --watch <name>` uses
to OPEN the stream feed also opens a CONTROL feed at `.ensemble/control/<name>.ndjson` (an `ndjson::Feed`).
When wired, the conductor spawns ONE background watcher thread that polls the control feed; a `Steer` pushes
its prompt onto a shared queue, an `Abort` sets the conductor's existing `abort` flag (and, for `--hard`, a
companion `hard` flag). At each round boundary the conductor drains the steer queue into `feedback` (so it
reaches the next round's prompts) and streams an `Injected` event; the abort flag (already checked at the
boundary) stops the run cleanly and streams an `Interrupted` event. For `--hard`, the adapters become
abort-aware: their existing poll loop also checks the shared abort flag and `kill_tree`s the child mid-turn.

**Tech Stack:** Rust 2021, sync, threads. Reuses `ndjson::Feed` + `RunObserver`/`StreamEvent` (S1a),
`exec_adapter::kill_tree` + the adapter poll loop, the conductor's `Arc<AtomicBool>` abort flag.

**Design decisions (locked):**
- One `<name>` drives observe AND control (`run --watch <name>` opens both feeds). Keeps the operator's
  mental model simple: `watch <name>`, `steer <name>`, `abort <name>`.
- `ControlCmd` is serde-tagged on `"cmd"` (snake_case), mirroring `StreamEvent`'s `"ev"` discipline.
- Steer is NEXT-ROUND (the current turn is a blocking one-shot — no mid-turn input channel; documented).
- Abort default = clean stop at the next round boundary (never discards completed+gated work that already
  passed). `--hard` = immediate kill_tree of the running CLI (current turn's partial work is lost).
- Best-effort + safe: a control-feed read failure never crashes a run; an abort can only STOP a run, never
  fabricate a land.

---

### Task 1: `ControlCmd` schema + parse (pure)

**Files:** Modify `src/supervise.rs`; Test in its `#[cfg(test)]`.

- [ ] **Step 1: failing test**

```rust
#[test]
fn control_cmd_roundtrips_tagged_on_cmd() {
    let s = ControlCmd::Steer { from: "main@conductor".into(), prompt: "skip the UI".into() };
    let line = serde_json::to_string(&s).unwrap();
    assert!(line.contains(r#""cmd":"steer""#), "got {line}");
    assert_eq!(serde_json::from_str::<ControlCmd>(&line).unwrap(), s);
    let a = ControlCmd::Abort { from: "main@conductor".into(), hard: true };
    assert!(serde_json::to_string(&a).unwrap().contains(r#""cmd":"abort""#));
    assert_eq!(serde_json::from_str::<ControlCmd>(&serde_json::to_string(&a).unwrap()).unwrap(), a);
}
```

- [ ] **Step 2: run → FAIL** (`ControlCmd` missing).
- [ ] **Step 3: implement** in `src/supervise.rs`:

```rust
/// One line in a member's control feed (.ensemble/control/<name>.ndjson). Tagged on "cmd".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ControlCmd {
    Steer { from: String, prompt: String },
    Abort { from: String, #[serde(default)] hard: bool },
}

/// The control feed path for `name` under `repo`, confined like the stream feed.
pub fn member_control_path(repo: &Path, name: &str) -> PathBuf {
    repo.join(".ensemble").join("control").join(format!("{}.ndjson", crate::journal::sanitize_slug(name)))
}
```

- [ ] **Step 4: run → PASS** (`cargo test --lib supervise`).
- [ ] **Step 5: commit** `feat(supervise): ControlCmd schema + member_control_path (S1b)`

---

### Task 2: control watcher (pure-ish, testable with a Feed)

**Files:** Modify `src/supervise.rs`; Test in its `#[cfg(test)]`.

A `ControlState` bundles the shared signals; a `drain_control` function applies new control lines to it.
This is the unit the conductor's watcher thread loops over — TDD'd without threads.

- [ ] **Step 1: failing test**

```rust
#[test]
fn drain_control_applies_steer_and_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let feed = crate::ndjson::Feed::open(tmp.path().join("c.ndjson"));
    feed.append(&serde_json::to_string(&ControlCmd::Steer { from: "m".into(), prompt: "focus".into() }).unwrap()).unwrap();
    feed.append(&serde_json::to_string(&ControlCmd::Abort { from: "m".into(), hard: true }).unwrap()).unwrap();
    let st = ControlState::default();
    let mut cursor = 0usize;
    drain_control(&feed, &mut cursor, &st);
    assert_eq!(cursor, 2, "cursor advanced past both");
    assert_eq!(st.take_steers(), vec!["focus".to_string()]);
    assert!(st.aborted());
    assert!(st.hard());
}
```

- [ ] **Step 2: run → FAIL.**
- [ ] **Step 3: implement** in `src/supervise.rs`:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shared signals a control watcher feeds and the conductor reads at round boundaries.
#[derive(Default)]
pub struct ControlState {
    abort: Arc<AtomicBool>,
    hard: Arc<AtomicBool>,
    steers: Mutex<Vec<String>>,
}
impl ControlState {
    pub fn abort_flag(&self) -> Arc<AtomicBool> { self.abort.clone() }
    pub fn aborted(&self) -> bool { self.abort.load(Ordering::Relaxed) }
    pub fn hard(&self) -> bool { self.hard.load(Ordering::Relaxed) }
    pub fn take_steers(&self) -> Vec<String> { std::mem::take(&mut *self.steers.lock().unwrap()) }
}

/// Apply every NEW control line at index >= `cursor` to `st`, advancing `cursor`. Unknown/torn lines
/// are skipped (forward-compat). Best-effort: a read error leaves state unchanged.
pub fn drain_control(feed: &crate::ndjson::Feed, cursor: &mut usize, st: &ControlState) {
    let lines = match feed.read_since(*cursor) { Ok(l) => l, Err(_) => return };
    for line in &lines {
        if let Ok(cmd) = serde_json::from_str::<ControlCmd>(line) {
            match cmd {
                ControlCmd::Steer { prompt, .. } => st.steers.lock().unwrap().push(prompt),
                ControlCmd::Abort { hard, .. } => {
                    if hard { st.hard.store(true, Ordering::Relaxed); }
                    st.abort.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    *cursor += lines.len();
}
```

- [ ] **Step 4: run → PASS.**
- [ ] **Step 5: commit** `feat(supervise): ControlState + drain_control watcher core (S1b)`

---

### Task 3: conductor integration — steer (next round) + clean abort + Injected/Interrupted events

**Files:** Modify `src/conductor.rs`, `src/supervise.rs` (StreamEvent already has Injected/Interrupted);
Test in conductor `#[cfg(test)]`.

The conductor gets an optional `Arc<ControlState>` + the control `Feed`. It spawns a watcher thread that
loops `drain_control` every ~200ms. At each round-boundary it drains steers into `feedback` and streams an
`Injected`; the abort flag (shared with `ControlState`) is already the conductor's `abort`, so the existing
boundary check stops cleanly — add an `Interrupted` stream there.

- [ ] **Step 1: failing test** — a steer queued before the run reaches the implementer's feedback; an
  abort flag set mid-run stops it and streams `interrupted`. (Model on the S1a `run_mirrors` test with a
  MockAdapter; drive `ControlState` directly rather than a real thread for determinism.)

```rust
#[test]
fn run_consumes_steers_and_aborts_from_control_state() {
    // ... build a MockAdapter crew (impl + reviewer) ...
    let ctrl = std::sync::Arc::new(crate::supervise::ControlState::default());
    ctrl.push_steer_for_test("focus on errors");        // a test-only helper to seed a steer
    let c = conductor.with_control(ctrl.clone());
    // implementer's prompt for round 0 should carry the steer text (assert via a recording adapter)
    // then set abort and assert the next boundary returns Escalated("aborted ...") with an interrupted stream
}
```

- [ ] **Step 2: run → FAIL** (`with_control` missing).
- [ ] **Step 3: implement**: add `control: Option<Arc<ControlState>>` + `with_control` builder; in `run()`,
  at the top of the loop drain `control.take_steers()` into `feedback` (+ `note(... "injected" ...)` /
  stream `Injected`); make the abort-flag boundary checks also `note(... "interrupted" ...)`. When a real
  control Feed is wired (binary side), spawn the watcher thread; in the lib test, seed `ControlState`
  directly so no thread/IO is needed. Reuse the conductor's `abort` as `ControlState::abort_flag()`.
- [ ] **Step 4: run → PASS** + full conductor suite green.
- [ ] **Step 5: commit** `feat(conductor): consume steer/abort from a ControlState (S1b)`

---

### Task 4: `--hard` mid-turn kill — adapter abort-awareness

**Files:** Modify `src/adapter.rs` (trait default), `src/exec_adapter.rs` + `src/agy_adapter.rs` (honor the
flag in the poll loop), `src/conductor.rs` (hand each adapter the abort flag before `run`); Test in
`exec_adapter` `#[cfg(test)]`.

- [ ] **Step 1: failing test** (exec_adapter): a long command + a shared abort flag that another thread
  sets after ~300ms → `run` returns `Flaked` quickly (well under the command's natural duration), proving
  the external abort kills mid-turn.
- [ ] **Step 2: run → FAIL.**
- [ ] **Step 3: implement**: add `fn set_abort(&self, _flag: Arc<AtomicBool>) {}` to the `Adapter` trait
  (default no-op; Mock/Remote inherit it). `ExecAdapter`/`AgyAdapter` store it (`Mutex<Option<Arc<AtomicBool>>>`)
  and, in their existing try_wait poll loop, `if flag.load() { kill_tree(...); return Flaked("aborted") }`.
  The conductor, before each `adapter.run`, calls `adapter.set_abort(self.abort.clone())` when a control
  state is wired. So a `--hard` abort (watcher sets the flag) kills the running CLI immediately; a clean
  abort is still seen only at the boundary.
- [ ] **Step 4: run → PASS** (exec_adapter abort test < ~1s; existing timeout test still green).
- [ ] **Step 5: commit** `feat(adapter): external abort flag kills a running turn (S1b --hard)`

---

### Task 5: `ensemble steer` / `ensemble abort` commands + wire the control feed (IO shell)

**Files:** Modify `src/main.rs` (dispatch + two commands + USAGE; open the control feed in `run_single`
when `--watch` is set, spawn the watcher); `src/lib.rs` (re-exports). Gate-reviewed + smoke-tested.

- [ ] `ensemble steer <name> "<prompt>" [--repo <p>] [--from <id>]` → append a `Steer` to
  `member_control_path(repo, name)`.
- [ ] `ensemble abort <name> [--hard] [--repo <p>] [--from <id>]` → append an `Abort`.
- [ ] In `run_single`, when `--watch <name>` is set, ALSO open the control feed + build the `ControlState`
  + hand it to the conductor (`with_control`) so a run is steerable/abortable by that name.
- [ ] USAGE lines; value-flag validation for `--from`.
- [ ] **Build the Windows binary + smoke** (two/three shells): `ensemble run "<task>" --crew <p> --repo
  <scratch> --watch fleet` in A; `ensemble watch fleet --follow` in B; `ensemble steer fleet "also handle
  empty input"` then `ensemble abort fleet` in C → the steer shows as `injected` in B and the run stops.
- [ ] **commit** `feat(steer/abort): ensemble steer|abort <name> drive a live run's control feed (S1b)`

---

### Task 6: Land — full suite, clippy, double-gate, merge

- [ ] `cargo test --lib` green + `cargo clippy --all-targets -- -D warnings` clean (WSL).
- [ ] Double-gate `git diff main...HEAD` (codex empty-temp-cwd + claude stdin); both `VERDICT: LGTM`. Bar:
  watcher thread can't deadlock/leak or change a run's outcome except to STOP it; abort never fabricates a
  land; steer reaches the next round only (no mid-turn injection claim); `member_control_path` confinement;
  `--hard` kill is prompt and the clean abort still stops at the boundary; thread joins/shutdown on run end.
- [ ] On both LGTM: ff-merge to `main`; backlog (S1b done, item-6 + S1c-cross-machine-control next) as a
  separate commit; push as <you>; delete slice; clean gate scratch.

## Self-review
- **Coverage:** observe (S1a ✓) + steer + interrupt(clean+hard) = the "防跑偏" Stage-1 verbs. "Configure"
  is the separate item-6 slice (per-agent model/args/timeout), built next.
- **Cross-platform:** file-plane control + the abort flag are OS-agnostic; `--hard` reuses the existing
  cross-platform `kill_tree` (Windows taskkill / Unix child.kill, both already cfg-gated). No ConPTY needed.
- **Honest limit:** steer is next-round, not mid-turn (the conductor's turns are blocking one-shots);
  true mid-turn inject is the later S2/S3 (M-API / M-PTY) work.
