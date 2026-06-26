# S1a — Conductor live stream (看得到 a governed run) Implementation Plan

> **For agentic workers:** TDD task-by-task. Pure logic in lib (TDD'd); the IO shell in main.rs is gate-reviewed + smoke-tested (project convention). Steps use `- [ ]` checkboxes.

**Goal:** Make a live `ensemble run` watchable in real time: the conductor mirrors every blackboard post into a per-run stream feed, and `ensemble watch <name> --follow` renders it as the run progresses — turning 0.5 live-supervision from "analyzed" into "usable + demoable".

**Architecture:** The conductor already funnels every meaningful event through `Blackboard::post(from, kind, body)` (implementer result, test result, each reviewer verdict, flake notes). S1a injects an optional `RunObserver` sink into the `Conductor`; a single `note()` helper does `bb.post(...)` AND mirrors the `Message` to the observer. The production observer (`FeedObserver`) appends the JSON `Message` to the S0 `ndjson::Feed` at `.ensemble/stream/<name>.ndjson`. `ensemble watch` (S0) gains a `Message`-render branch so the same viewer tails BOTH member-session `StreamEvent`s and governed-run `Message`s. File-only — zero PTY, zero platform risk. No mid-turn token streaming (that is later S2/item-10) — granularity is turn/post level, which is the right level to supervise a governed run.

**Tech Stack:** Rust 2021, sync. Reuses `ndjson::Feed` + `ensemble watch` (S0), `blackboard::Message`, the existing conductor run loop.

**Design decisions (locked):**
- The live feed carries `Message` JSON (`{"from","kind","body"}`), NOT `StreamEvent`. A governed run is multi-agent; `Message` is its natural unit. `ensemble watch` disambiguates by shape: `"ev"` ⇒ `StreamEvent`; else `from/kind/body` ⇒ `Message`; else raw. So one viewer tails both.
- The feed NAME is operator-chosen via `ensemble run --watch <name>` (absent ⇒ no live feed, current behaviour). Feed path = `member_stream_path(repo, name)` — under the stable REPO `.ensemble/stream/`, not the throwaway worktree.
- The observer is best-effort: a feed-write failure NEVER changes the run outcome (mirrors journal's `let _ =` discipline).
- A synthetic terminal `Message { from:"conductor", kind:"decision", body:<reason> }` is streamed at the end so a watcher sees LANDED/escalated live.

---

### Task 1: `ensemble watch` renders a `Message` line

**Files:**
- Modify: `src/supervise.rs` (`render_line`)
- Test: `src/supervise.rs` `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn render_line_pretty_prints_a_blackboard_message() {
    // a governed-run blackboard post (no "ev" tag) renders as "[from · kind] body", not raw
    let raw = r#"{"from":"codex","kind":"result","body":"implemented the parser"}"#;
    let s = render_line(raw);
    assert!(s.contains("codex") && s.contains("result"), "got {s}");
    assert!(s.contains("implemented the parser"), "got {s}");
    assert!(!s.starts_with('?'), "a valid Message must not fall back to raw: {s}");
    // a StreamEvent still wins (more specific, tagged on "ev")
    let ev = r#"{"ev":"turn_start","n":1,"prompt":"do it","ts":"T"}"#;
    assert!(render_line(ev).contains("turn #1"));
    // genuine garbage still falls back to raw
    assert!(render_line("not json").starts_with('?'));
}
```

- [ ] **Step 2: Run it — expect FAIL** (the Message renders as `? {raw}` today).

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib render_line_pretty_prints_a_blackboard_message'`
Expected: FAIL (assertion: falls back to raw).

- [ ] **Step 3: Implement** — add a `Message` branch to `render_line`:

```rust
pub fn render_line(raw: &str) -> String {
    if let Ok(ev) = serde_json::from_str::<StreamEvent>(raw) {
        return render_event(&ev);
    }
    if let Ok(m) = serde_json::from_str::<crate::blackboard::Message>(raw) {
        return format!("  [{} · {}] {}", m.from, m.kind, inline(&m.body));
    }
    format!("? {}", raw.trim())
}
```

- [ ] **Step 4: Run — expect PASS**, plus the existing supervise tests still green.

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`
Expected: PASS.

- [ ] **Step 5: Commit** `git add src/supervise.rs` → `feat(supervise): ensemble watch renders blackboard Message lines (S1a)`

---

### Task 2: `RunObserver` sink + conductor mirrors its posts

**Files:**
- Modify: `src/supervise.rs` (add `RunObserver` trait)
- Modify: `src/conductor.rs` (field + `with_stream` builder + `note()` helper; route the run loop's posts)
- Test: `src/conductor.rs` `#[cfg(test)]`

- [ ] **Step 1: Add the trait** (in `src/supervise.rs`):

```rust
/// A live sink the conductor mirrors each blackboard post into, so a run is watchable in real time.
pub trait RunObserver: Send + Sync {
    fn post(&self, m: &crate::blackboard::Message);
}
```

- [ ] **Step 2: Write the failing test** (in `src/conductor.rs` tests — reuse the existing `MockAdapter` crew helpers):

```rust
#[test]
fn run_mirrors_blackboard_posts_to_the_observer_in_order() {
    use std::sync::Mutex;
    struct Rec(Mutex<Vec<(String, String)>>); // (from, kind)
    impl crate::supervise::RunObserver for Rec {
        fn post(&self, m: &crate::blackboard::Message) {
            self.0.lock().unwrap().push((m.from.clone(), m.kind.clone()));
        }
    }
    let rec = std::sync::Arc::new(Rec(Mutex::new(Vec::new())));
    // a crew that lands in one round: impl -> one approving reviewer (min_approvals=1)
    let c = land_in_one_round_conductor().with_stream(Box::new(ObserverArc(rec.clone())));
    let out = c.run("do the thing", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed));
    let seen = rec.0.lock().unwrap().clone();
    // the implementer's result and the reviewer's verdict were both streamed, in order
    assert!(seen.iter().any(|(_, k)| k == "result"), "result streamed: {seen:?}");
    assert!(seen.iter().any(|(_, k)| k == "verdict"), "verdict streamed: {seen:?}");
    // and a terminal decision message closes the stream
    assert!(seen.iter().any(|(f, k)| f == "conductor" && k == "decision"), "decision streamed: {seen:?}");
}
```

(Helper `land_in_one_round_conductor()` builds a `Conductor` from `MockAdapter`s — model it on the existing conductor tests; `ObserverArc` is a thin `Arc<dyn RunObserver>` newtype so the test can read what was streamed after the run. If the existing tests already expose a one-round-land fixture, reuse it.)

- [ ] **Step 3: Run — expect FAIL** (`with_stream` doesn't exist; the conductor doesn't mirror).

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib run_mirrors_blackboard_posts'`
Expected: FAIL (compile error on `with_stream` → it's the RED signal; implement to GREEN).

- [ ] **Step 4: Implement** in `src/conductor.rs`:

```rust
// field on Conductor:
stream: Option<Box<dyn crate::supervise::RunObserver>>,
// initialise to None in `new`; add the builder:
pub fn with_stream(mut self, obs: Box<dyn crate::supervise::RunObserver>) -> Self {
    self.stream = Some(obs);
    self
}
// the single funnel — replaces direct bb.post(...) calls in `run`:
fn note(&self, bb: &mut Blackboard, from: &str, kind: &str, body: &str) {
    bb.post(from, kind, body);
    if let Some(s) = &self.stream {
        s.post(&crate::blackboard::Message {
            from: from.to_string(),
            kind: kind.to_string(),
            body: body.to_string(),
        });
    }
}
```

Then in `run`: replace every `bb.post(a, k, b)` with `self.note(&mut bb, a, k, b)`. At EACH terminal `return RunOutcome { decision: ... }`, first stream the decision — simplest is a small helper that, given the `Decision`, calls `self.note(&mut bb, "conductor", "decision", &reason)` before returning (reason = "LANDED" or the escalation string). Keep it to the Land and Escalate exits.

- [ ] **Step 5: Run — expect PASS**, plus the full conductor suite green.

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib conductor'`
Expected: PASS.

- [ ] **Step 6: Commit** `git add src/supervise.rs src/conductor.rs` → `feat(conductor): mirror blackboard posts to an optional RunObserver (S1a)`

---

### Task 3: `FeedObserver` + `ensemble run --watch <name>` wiring (IO shell)

**Files:**
- Modify: `src/supervise.rs` (add `FeedObserver`)
- Modify: `src/lib.rs` (re-export `RunObserver`, `FeedObserver`)
- Modify: `src/main.rs` (`run_single`: parse `--watch`, build the feed observer, `.with_stream(...)`; USAGE line)
- Test: `src/supervise.rs` `#[cfg(test)]` (FeedObserver appends a parseable Message line)

- [ ] **Step 1: Write the failing test** (FeedObserver is testable — `Feed` is):

```rust
#[test]
fn feed_observer_appends_a_parseable_message_line() {
    let tmp = tempfile::tempdir().unwrap();
    let obs = FeedObserver::new(Feed::open(tmp.path().join("run.ndjson")));
    obs.post(&crate::blackboard::Message { from: "codex".into(), kind: "result".into(), body: "x".into() });
    let lines = Feed::open(tmp.path().join("run.ndjson")).read_since(0).unwrap();
    assert_eq!(lines.len(), 1);
    assert!(render_line(&lines[0]).contains("codex"), "rendered: {}", render_line(&lines[0]));
}
```

- [ ] **Step 2: Run — expect FAIL** (`FeedObserver` missing).

- [ ] **Step 3: Implement** in `src/supervise.rs`:

```rust
/// A `RunObserver` that mirrors each post into an `ndjson::Feed` for `ensemble watch` to tail.
/// Best-effort: a write failure is swallowed so it never changes a run's outcome.
pub struct FeedObserver {
    feed: crate::ndjson::Feed,
}
impl FeedObserver {
    pub fn new(feed: crate::ndjson::Feed) -> Self {
        Self { feed }
    }
}
impl RunObserver for FeedObserver {
    fn post(&self, m: &crate::blackboard::Message) {
        if let Ok(line) = serde_json::to_string(m) {
            let _ = self.feed.append(&line);
        }
    }
}
```

Re-export in `src/lib.rs`: add `FeedObserver, RunObserver` to the `pub use supervise::{...}` line.

- [ ] **Step 4: Run — expect PASS**.

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`

- [ ] **Step 5: Wire `--watch` in `src/main.rs` `run_single`** (IO shell — gate-reviewed, not unit-tested):

After `let repo = parse_flag(args, "--repo")...;` and after `Conductor::new(...).with_abort(...)`, branch on `--watch`:

```rust
let c = Conductor::new(crew, registry).with_abort(abort_flag());
let c = match ensemble::parse_flag_pub(args, "--watch") {
    Some(name) => {
        let feed = ensemble::Feed::open(ensemble::member_stream_path(std::path::Path::new(&repo), &name));
        eprintln!("ensemble run: live stream → ensemble watch {name} --follow");
        c.with_stream(Box::new(ensemble::FeedObserver::new(feed)))
    }
    None => c,
};
```

(If `parse_flag` isn't already `pub`, reuse the existing arg accessor the file uses — match the surrounding code; `--watch` takes a value, so also add it to the value-flag validation if the file validates those.) Add to `USAGE` the `run` line: `[--watch <name>]`.

- [ ] **Step 6: Build the Windows binary + smoke**

Build: `$env:CARGO_TARGET_DIR="C:\ctgt\ensemble"; cargo build --release`
Smoke (two shells): terminal A `ensemble run "<trivial task>" --crew <p> --repo <scratch> --watch demo --merge`; terminal B `ensemble watch demo --follow` → the run's `[codex · result]`, `[claude · verdict]`, `[conductor · decision] LANDED` appear LIVE as the run progresses.

- [ ] **Step 7: Commit** `git add src/supervise.rs src/lib.rs src/main.rs` → `feat(run): --watch <name> streams a governed run live for ensemble watch (S1a)`

---

### Task 4: Land — full suite, clippy, double-gate, merge

- [ ] `cargo test --lib` (all green) + `cargo clippy --all-targets -- -D warnings` (clean) via WSL.
- [ ] Double-gate `git diff main...HEAD` (codex from the empty temp gate cwd + claude via stdin); both must end `VERDICT: LGTM`. Bar: observer never alters run outcome (best-effort), no deadlock/borrow issue from `&self` streaming, render disambiguation (StreamEvent vs Message vs raw) correct, `--watch` arg parsing + feed-path confinement (reuses `member_stream_path`).
- [ ] On both LGTM: ff-merge the slice to `main`; update `docs/AUTONOMOUS-BACKLOG.md` (S1a done, S1 control plane next) as a separate commit; push as <you>; delete the slice branch; clean gate scratch.

## Self-review
- **Spec coverage:** S1a = the "看得到" half of S1 (observe). Control plane (steer/abort) + serve routes are the NEXT S1 sub-slice, not here. ✓
- **Type consistency:** `RunObserver::post(&Message)` used identically in Task 2 (trait), Task 2 (conductor `note`), Task 3 (FeedObserver). `member_stream_path`, `Feed`, `render_line` reused from S0. ✓
- **No placeholder:** every step has concrete code/commands. The one soft spot — the exact `parse_flag`/value-flag accessor name in main.rs — is resolved by matching the surrounding code at implementation time (the file already parses `--crew`/`--repo` the same way).
