# S0 — Supervision File-Plane (stream feed + `ensemble watch`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land S0 of live supervision (design: `docs/specs/2026-06-21-live-supervision-design.md`): a generic append-only NDJSON feed primitive, the `StreamEvent` schema + renderer, member-path confinement, and a read-only `ensemble watch <member>` that tails a member's stream feed.

**Architecture:** Pure logic lives in lib (`ndjson.rs` feed primitive, `supervise.rs` schema/render/path/arg-parse) and is TDD'd; the IO shell (`watch_cmd`) lives in `main.rs` and is gate-reviewed + smoke-tested (established project convention). The feed primitive deliberately mirrors the proven locking discipline of `board.rs` (fs2 exclusive-append / shared-read / torn-tail repair / lossless cursor) generalized over arbitrary JSON lines; `board.rs` is left untouched (YAGNI — it could later be refactored onto `Feed`). No backend produces events in S0 — `watch` tails a feed written manually (smoke) or by later slices (conductor in S1, backends in S2/S3).

**Tech Stack:** Rust 2021, single binary, synchronous. Deps already in tree: `serde`, `serde_json`, `fs2`, `tempfile` (dev). No `Cargo.toml` change.

**Build/test commands:**
- Lib tests (WSL, avoids Defender lock): `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib <filter>'`
- Clippy: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo clippy --all-targets -- -D warnings'`
- Binary (Windows native, for smoke): `$env:CARGO_TARGET_DIR="C:\ctgt\ensemble"; cargo build` → `C:\ctgt\ensemble\debug\ensemble.exe`

**Landing:** after all tasks green, commit specific files to a slice branch, run the double-gate (codex from empty temp cwd + claude), ff-merge to main ONLY after BOTH end `VERDICT: LGTM`. Identity `<you> <you@example.com>`.

---

### Task 1: `ndjson::Feed` primitive

**Files:**
- Create: `src/ndjson.rs`
- Modify: `src/lib.rs` (declare module + re-export)
- Test: in-file `#[cfg(test)] mod tests`

- [ ] **Step 1: Declare the module + re-export (so the new file compiles into the crate)**

In `src/lib.rs`, add `pub mod ndjson;` after `pub mod mesh;` (line 19), and `pub use ndjson::Feed;` in the re-export block (after `pub use mesh::{...};`, line 43).

- [ ] **Step 2: Write the failing tests**

Create `src/ndjson.rs` with ONLY the tests first (the `Feed` type doesn't exist yet → compile-fail = RED):

```rust
//! A generic append-only NDJSON feed: one JSON object per line, multi-process safe. Generalizes the
//! locking discipline proven in `board.rs` (an `fs2` exclusive lock serializes appends, a shared lock
//! guards reads, a torn trailing line is repaired before the next append, and bad lines are skipped)
//! over ARBITRARY JSON lines — the caller serde-encodes/decodes its own record type. Backs the
//! live-supervision feeds (.ensemble/stream/<member>.ndjson, control/<member>.ndjson). NOTE: mirrors
//! board.rs:115-141 deliberately; board.rs is left untouched (it could later move onto this primitive).

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(dir: &std::path::Path, name: &str) -> Feed {
        Feed::open(dir.join(name))
    }

    #[test]
    fn append_then_read_roundtrips_with_a_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        assert!(f.is_empty().unwrap());
        let c1 = f.append(r#"{"ev":"a","n":1}"#).unwrap();
        let c2 = f.append(r#"{"ev":"b","n":2}"#).unwrap();
        assert_eq!((c1, c2), (1, 2), "cursor sits one past each own line");
        let all = f.read_since(0).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].contains(r#""ev":"a""#));
        assert!(all[1].contains(r#""ev":"b""#));
        assert_eq!(f.read_since(1).unwrap().len(), 1, "cursor skips earlier lines");
    }

    #[test]
    fn read_missing_feed_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "nope.ndjson");
        assert!(f.read_since(0).unwrap().is_empty());
        assert_eq!(f.len().unwrap(), 0);
    }

    #[test]
    fn append_rejects_non_json_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        assert!(f.append("not json").is_err());
        assert!(f.read_since(0).unwrap().is_empty(), "a rejected append must write nothing");
    }

    #[test]
    fn torn_tail_is_repaired_so_the_next_append_is_not_swallowed() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        f.append(r#"{"ev":"first"}"#).unwrap();
        let mut raw = std::fs::OpenOptions::new().append(true).open(f.path()).unwrap();
        raw.write_all(br#"{"ev":"torn"#).unwrap(); // partial line, no newline
        drop(raw);
        assert_eq!(f.read_since(0).unwrap().len(), 1, "the torn partial line is skipped");
        f.append(r#"{"ev":"third"}"#).unwrap();
        let all = f.read_since(0).unwrap();
        assert_eq!(all.len(), 2, "torn line skipped; both real records kept");
        assert!(all[1].contains("third"));
    }

    #[test]
    fn a_bad_middle_line_is_skipped_without_hiding_later_lines() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        f.append(r#"{"ev":"one"}"#).unwrap();
        let mut raw = std::fs::OpenOptions::new().append(true).open(f.path()).unwrap();
        raw.write_all(&[0xff, 0xfe, b'\n']).unwrap(); // non-UTF-8 / non-JSON, newline-terminated
        drop(raw);
        f.append(r#"{"ev":"three"}"#).unwrap();
        let all = f.read_since(0).unwrap();
        assert_eq!(all.len(), 2, "bad middle line skipped: {all:?}");
        assert!(all[0].contains("one") && all[1].contains("three"));
    }

    #[test]
    fn concurrent_appends_serialize_with_no_loss() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::thread::scope(|s| {
            for i in 0..20 {
                let dir = dir.clone();
                s.spawn(move || {
                    Feed::open(dir.join("s.ndjson"))
                        .append(&format!(r#"{{"ev":"w","i":{i}}}"#))
                        .unwrap();
                });
            }
        });
        let all = Feed::open(dir.join("s.ndjson")).read_since(0).unwrap();
        assert_eq!(all.len(), 20, "every concurrent append survives");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail (RED)**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib ndjson'`
Expected: FAIL — `cannot find type Feed` (the type doesn't exist yet).

- [ ] **Step 4: Implement `Feed` (minimal to pass)**

Prepend to `src/ndjson.rs` (above the test module):

```rust
use fs2::FileExt;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// An append-only NDJSON feed at one path.
pub struct Feed {
    path: PathBuf,
}

impl Feed {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one JSON line under an EXCLUSIVE lock, creating the parent dir if needed, and return the
    /// feed CURSOR (count of valid JSON lines) read back while the lock is STILL HELD — so a poller of
    /// `read_since(cursor)` never skips a concurrently-appended line (the lossless-cursor guarantee from
    /// board.rs). Rejects a `line` that isn't a single valid JSON value (so we never persist a record we
    /// couldn't read back). A trailing newline on `line` is normalized; exactly one is written.
    pub fn append(&self, line: &str) -> std::io::Result<usize> {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if serde_json::from_str::<Value>(trimmed).is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ndjson append: line is not valid JSON",
            ));
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut buf = trimmed.to_string();
        buf.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        f.lock_exclusive()?;
        let r = append_terminated(&mut f, buf.as_bytes()).and_then(|()| count_valid(&mut f));
        let _ = f.unlock();
        r
    }

    /// All valid JSON lines at index ≥ `n`, in append order, under a SHARED lock (so a concurrent
    /// append can't expose a half-written line). Torn/non-JSON lines are skipped individually. Empty if
    /// the feed doesn't exist yet.
    pub fn read_since(&self, n: usize) -> std::io::Result<Vec<String>> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        f.lock_shared()?;
        let mut bytes = Vec::new();
        let read = f.read_to_end(&mut bytes);
        let _ = f.unlock();
        read?;
        let lines: Vec<String> = bytes
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .filter(|l| serde_json::from_slice::<Value>(l).is_ok())
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect();
        Ok(lines.into_iter().skip(n).collect())
    }

    /// Total valid-line count — the cursor a poller advances to.
    pub fn len(&self) -> std::io::Result<usize> {
        Ok(self.read_since(0)?.len())
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Append `line` to `f` (held under an exclusive lock, opened read+append), repairing a torn tail
/// first: if the file is non-empty and does NOT end in `\n`, terminate the partial line before
/// appending so O_APPEND can't weld our record onto it. (Mirrors board.rs::append_terminated.)
fn append_terminated(f: &mut std::fs::File, line: &[u8]) -> std::io::Result<()> {
    let len = f.metadata()?.len();
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    f.write_all(line)
}

/// Count valid JSON lines in `f` (held under the exclusive lock), using the SAME non-empty + JSON-
/// parseable filter as `read_since`, so the count equals the cursor a reader polls from. Seeks to the
/// start first (the caller left the position at EOF after appending).
fn count_valid(f: &mut std::fs::File) -> std::io::Result<usize> {
    f.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .filter(|l| serde_json::from_slice::<Value>(l).is_ok())
        .count())
}
```

- [ ] **Step 5: Run the tests to verify they pass (GREEN)**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib ndjson'`
Expected: PASS (6 tests). Output pristine.

- [ ] **Step 6: Commit**

```bash
git add src/ndjson.rs src/lib.rs
git commit -F <msg-file>   # "feat(supervise): ndjson::Feed append-only feed primitive (S0)"
```

---

### Task 2: `StreamEvent` schema + `render_line`

**Files:**
- Create: `src/supervise.rs`
- Modify: `src/lib.rs` (declare module + re-export)
- Test: in-file `#[cfg(test)] mod tests`

- [ ] **Step 1: Declare the module + re-export**

In `src/lib.rs`, add `pub mod supervise;` after `pub mod serve;` (line 22), and `pub use supervise::{member_stream_path, parse_watch_args, render_event, render_line, StreamEvent, WatchArgs};` after `pub use serve::{...};` (line 49). (The path/arg items land in Tasks 3–4; declaring them now keeps one edit — they will exist before the final build. If executing strictly task-by-task, add only `render_event, render_line, StreamEvent` here and extend in Tasks 3/4.)

- [ ] **Step 2: Write the failing tests**

Create `src/supervise.rs` with the module doc + tests first (types/fns missing → RED):

```rust
//! Live-supervision schema and helpers (design: docs/specs/2026-06-21-live-supervision-design.md). The
//! stream feed (.ensemble/stream/<member>.ndjson) carries `StreamEvent`s a supervisor tees from a live
//! CLI session; `ensemble watch` renders them. Pure (schema + render + path confinement + arg parse);
//! the IO shell lives in main.rs.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_event_roundtrips_with_the_ev_tag() {
        let ev = StreamEvent::TurnStart { n: 7, prompt: "do the auth path".into(), ts: "T".into() };
        let line = serde_json::to_string(&ev).unwrap();
        assert!(line.contains(r#""ev":"turn_start""#), "got {line}");
        assert!(line.contains(r#""n":7"#));
        let back: StreamEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn injected_event_roundtrips() {
        let ev = StreamEvent::Injected {
            n: 8, from: "main@node-a".into(), prompt: "focus".into(), ts: "T".into(),
        };
        let back: StreamEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn render_line_pretty_prints_a_known_event() {
        let raw = r#"{"ev":"injected","n":8,"from":"main@conductor","prompt":"skip the UI","ts":"T"}"#;
        let s = render_line(raw);
        assert!(s.contains("inject #8"), "got {s}");
        assert!(s.contains("main@conductor") && s.contains("skip the UI"), "got {s}");
    }

    #[test]
    fn render_line_falls_back_on_unknown_or_torn_lines() {
        // a forward-compat event kind this binary doesn't know must NOT be hidden
        let future = r#"{"ev":"some_future_kind","x":1}"#;
        assert!(render_line(future).contains("some_future_kind"), "unknown kind shown raw");
        // a valid-JSON but non-event line is shown raw, not dropped
        assert!(render_line("{}").starts_with('?'));
    }
}
```

- [ ] **Step 3: Run to verify RED**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`
Expected: FAIL — `cannot find type StreamEvent` / `cannot find function render_line`.

- [ ] **Step 4: Implement the schema + renderer**

Prepend to `src/supervise.rs` (above the tests):

```rust
use serde::{Deserialize, Serialize};

/// One line in a member's stream feed. Internally tagged on `"ev"` (like journal::Entry's `"rec"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum StreamEvent {
    SessionStart {
        member: String,
        cli: String,
        backend: String,
        #[serde(default)]
        host: Option<String>,
        pid: u32,
        ts: String,
    },
    TurnStart { n: u64, #[serde(default)] prompt: String, ts: String },
    Output { n: u64, text: String, ts: String },
    Tool { n: u64, name: String, #[serde(default)] detail: String, ts: String },
    TurnEnd { n: u64, reply: String, ts: String },
    Injected { n: u64, from: String, prompt: String, ts: String },
    Interrupted { n: u64, from: String, hard: bool, ts: String },
    SessionEnd { reason: String, ts: String },
}

/// Render one stream event as a single human line for `ensemble watch`.
pub fn render_event(ev: &StreamEvent) -> String {
    match ev {
        StreamEvent::SessionStart { member, cli, backend, host, pid, .. } => {
            let h = host.as_deref().unwrap_or("?");
            format!("● session_start  {member} ({cli}/{backend}) host={h} pid={pid}")
        }
        StreamEvent::TurnStart { n, prompt, .. } => format!("▶ turn #{n} start  {}", inline(prompt)),
        StreamEvent::Output { n, text, .. } => format!("  #{n} | {}", inline(text)),
        StreamEvent::Tool { n, name, detail, .. } => format!("  #{n} ⚙ {name}  {}", inline(detail)),
        StreamEvent::TurnEnd { n, reply, .. } => format!("◀ turn #{n} end    {}", inline(reply)),
        StreamEvent::Injected { n, from, prompt, .. } => {
            format!("⤵ inject #{n} from {from}: {}", inline(prompt))
        }
        StreamEvent::Interrupted { n, from, hard, .. } => {
            format!("✖ interrupt #{n} from {from} ({})", if *hard { "hard" } else { "ctrl-c" })
        }
        StreamEvent::SessionEnd { reason, .. } => format!("● session_end  ({reason})"),
    }
}

/// Render one RAW feed line: parse → pretty render; on ANY parse failure (a torn line, or a forward-
/// compat event kind this binary doesn't know) fall back to the raw line so nothing is hidden.
pub fn render_line(raw: &str) -> String {
    match serde_json::from_str::<StreamEvent>(raw) {
        Ok(ev) => render_event(&ev),
        Err(_) => format!("? {}", raw.trim()),
    }
}

/// Collapse a possibly-multiline excerpt to one whitespace-normalized line, bounded for the watch view.
fn inline(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 120 {
        one
    } else {
        format!("{}…", one.chars().take(120).collect::<String>())
    }
}
```

- [ ] **Step 5: Run to verify GREEN**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add src/supervise.rs src/lib.rs
git commit -F <msg-file>   # "feat(supervise): StreamEvent schema + render_line (S0)"
```

---

### Task 3: member-path confinement (`member_stream_path`)

**Files:**
- Modify: `src/journal.rs` (widen `sanitize_slug` visibility)
- Modify: `src/supervise.rs` (add `member_stream_path` + tests)

- [ ] **Step 1: Write the failing tests**

Add to `src/supervise.rs`'s `mod tests` (and add `use std::path::Path;` inside the test module if not present):

```rust
    #[test]
    fn member_stream_path_confines_a_hostile_member() {
        use std::path::Path;
        let repo = Path::new("/tmp/repo");
        let stream = repo.join(".ensemble").join("stream");
        let p = member_stream_path(repo, "../../etc/passwd");
        assert!(p.starts_with(&stream), "member must not escape the stream dir: {p:?}");
        assert_eq!(p.parent().unwrap(), stream, "must be a direct child of stream/");
        assert!(!p.components().any(|c| c.as_os_str() == ".."), "no traversal survives: {p:?}");
    }

    #[test]
    fn member_stream_path_sanitizes_the_member_into_one_component() {
        use std::path::Path;
        // reuses journal's sanitizer: '@' in the canonical <cli>@<host> name becomes '-' on disk; the
        // supervisor and `watch` compute the SAME path, so the logical member id still resolves.
        let p = member_stream_path(Path::new("/r"), "claude@conductor");
        assert!(p.ends_with("claude-conductor.ndjson"), "got {p:?}");
    }
```

- [ ] **Step 2: Run to verify RED**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`
Expected: FAIL — `cannot find function member_stream_path`.

- [ ] **Step 3: Widen the shared sanitizer**

In `src/journal.rs:56`, change the signature (single source of truth for path-component sanitization):

```rust
pub(crate) fn sanitize_slug(slug: &str) -> String {
```

(Leave the body and its existing tests unchanged.)

- [ ] **Step 4: Implement `member_stream_path`**

Append to the non-test part of `src/supervise.rs`:

```rust
use std::path::{Path, PathBuf};

/// The stream feed path for `member` under `repo`, confined to `<repo>/.ensemble/stream/`. `member`
/// comes from untrusted input (argv / HTTP path), so it is reduced to ONE safe filename component by
/// journal's shared path-component sanitizer.
pub fn member_stream_path(repo: &Path, member: &str) -> PathBuf {
    repo.join(".ensemble")
        .join("stream")
        .join(format!("{}.ndjson", crate::journal::sanitize_slug(member)))
}
```

- [ ] **Step 5: Run to verify GREEN**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise journal'`
Expected: PASS (supervise: 6 tests; journal tests still green).

- [ ] **Step 6: Commit**

```bash
git add src/journal.rs src/supervise.rs
git commit -F <msg-file>   # "feat(supervise): member_stream_path confinement via shared sanitizer (S0)"
```

---

### Task 4: `parse_watch_args` (pure argv parse)

**Files:**
- Modify: `src/supervise.rs` (add `WatchArgs` + `parse_watch_args` + tests)

- [ ] **Step 1: Write the failing tests**

Add to `src/supervise.rs`'s `mod tests`:

```rust
    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_watch_args_basic() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "claude@conductor"]));
        assert_eq!(w.member.as_deref(), Some("claude@conductor"));
        assert_eq!(w.since, 0);
        assert!(!w.follow);
        assert_eq!(w.repo, None);
    }

    #[test]
    fn parse_watch_args_all_flags() {
        let w = parse_watch_args(&argv(&[
            "ensemble", "watch", "--since", "5", "claude@conductor", "--follow", "--repo", "/r",
        ]));
        assert_eq!(w.member.as_deref(), Some("claude@conductor"));
        assert_eq!(w.since, 5);
        assert!(w.follow);
        assert_eq!(w.repo.as_deref(), Some("/r"));
    }

    #[test]
    fn parse_watch_args_since_nonnumber_falls_back_to_zero() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "m", "--since", "abc"]));
        assert_eq!(w.since, 0);
    }

    #[test]
    fn parse_watch_args_missing_member_is_none() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "--follow"]));
        assert_eq!(w.member, None);
    }
```

- [ ] **Step 2: Run to verify RED**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`
Expected: FAIL — `cannot find type WatchArgs` / `cannot find function parse_watch_args`.

- [ ] **Step 3: Implement**

Append to the non-test part of `src/supervise.rs`:

```rust
/// Parsed `ensemble watch` arguments (pure; the IO shell in main.rs consumes this).
#[derive(Debug, PartialEq)]
pub struct WatchArgs {
    pub member: Option<String>,
    pub repo: Option<String>,
    pub since: usize,
    pub follow: bool,
}

/// Parse the argv of `ensemble watch <member> [--repo <p>] [--since <n>] [--follow]`. `args` is the full
/// process argv (args[0]=exe, args[1]="watch"). The first non-flag token is the member; unknown
/// `--flags` are skipped (no value assumed); a non-numeric `--since` falls back to 0.
pub fn parse_watch_args(args: &[String]) -> WatchArgs {
    let mut out = WatchArgs { member: None, repo: None, since: 0, follow: false };
    let mut i = 2; // skip exe + "watch"
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => { out.repo = args.get(i + 1).cloned(); i += 2; }
            "--since" => { out.since = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0); i += 2; }
            "--follow" => { out.follow = true; i += 1; }
            a if a.starts_with("--") => { i += 1; }
            _ => { if out.member.is_none() { out.member = Some(args[i].clone()); } i += 1; }
        }
    }
    out
}
```

- [ ] **Step 4: Run to verify GREEN**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib supervise'`
Expected: PASS (10 supervise tests total).

- [ ] **Step 5: Commit**

```bash
git add src/supervise.rs
git commit -F <msg-file>   # "feat(supervise): parse_watch_args (S0)"
```

---

### Task 5: `ensemble watch` IO shell + dispatch + USAGE

**Files:**
- Modify: `src/main.rs` (dispatch arm, `watch_cmd`, USAGE line)
- (Re-exports from `src/lib.rs` already added in Tasks 2–4: `Feed`, `member_stream_path`, `parse_watch_args`, `render_line`.)

This task is the IO shell — NOT unit-tested (project convention); verified by build + clippy + smoke.

- [ ] **Step 1: Add the dispatch arm**

In `src/main.rs`, in the `match sub` block, add after `Some("up") => up_cmd(&args),` (line 55):

```rust
        Some("watch") => watch_cmd(&args),
```

- [ ] **Step 2: Add a USAGE line**

In the `USAGE` const, add (after the `mcp install` line, before the trailing discovery note):

```
    ensemble watch <member> [--repo <p>] [--since <n>] [--follow]   (tail a live member's stream feed)\n  \
```

- [ ] **Step 3: Implement `watch_cmd`**

Add a new function in `src/main.rs` (near the other `*_cmd` handlers):

```rust
/// `ensemble watch <member> [--repo <p>] [--since <n>] [--follow]` — tail a member's stream feed
/// (.ensemble/stream/<member>.ndjson), rendering each event. Read-only. `--follow` polls for new events
/// until Ctrl-C. (S0 of live supervision: local tail; remote `--node` tail arrives in S1.)
fn watch_cmd(args: &[String]) {
    let w = ensemble::parse_watch_args(args);
    let member = match w.member {
        Some(m) => m,
        None => {
            eprintln!("usage: ensemble watch <member> [--repo <p>] [--since <n>] [--follow]");
            std::process::exit(2);
        }
    };
    let repo = w
        .repo
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    let feed = ensemble::Feed::open(ensemble::member_stream_path(&repo, &member));

    let mut cursor = w.since;
    let drain = |cursor: &mut usize| match feed.read_since(*cursor) {
        Ok(lines) => {
            for l in &lines {
                println!("{}", ensemble::render_line(l));
            }
            *cursor += lines.len();
            true
        }
        Err(e) => {
            eprintln!("ensemble watch: read {}: {e}", feed.path().display());
            false
        }
    };

    if !drain(&mut cursor) {
        std::process::exit(1);
    }
    if !w.follow {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !drain(&mut cursor) {
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 4: Build + clippy**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo clippy --all-targets -- -D warnings'`
Expected: clean (no warnings). Then build the binary for smoke: `$env:CARGO_TARGET_DIR="C:\ctgt\ensemble"; cargo build` (Windows).

- [ ] **Step 5: Smoke (manual feed → watch → since → follow)**

In a scratch dir (NOT the repo root — keep `.ensemble/` scratch out of the tree), write a stream feed by hand and tail it:

```powershell
$ex = "C:\ctgt\ensemble\debug\ensemble.exe"
$d  = "$env:TEMP\ens_watch_smoke"; New-Item -ItemType Directory -Force "$d\.ensemble\stream" | Out-Null
$f  = "$d\.ensemble\stream\claude-conductor.ndjson"
Set-Content -Path $f -Encoding utf8 -Value @(
  '{"ev":"session_start","member":"claude@conductor","cli":"claude","backend":"pty","host":"conductor","pid":42,"ts":"T0"}',
  '{"ev":"turn_start","n":1,"prompt":"do the auth path","ts":"T1"}',
  '{"ev":"output","n":1,"text":"editing src/auth.rs","ts":"T2"}',
  '{"ev":"turn_end","n":1,"reply":"done","ts":"T3"}'
)
& $ex watch "claude@conductor" --repo $d            # expect 4 rendered lines
& $ex watch "claude@conductor" --repo $d --since 2  # expect last 2 lines only
```
Expected: member `claude@conductor` resolves to `claude-conductor.ndjson`; events render (`● session_start …`, `▶ turn #1 …`, `  #1 | editing …`, `◀ turn #1 end …`); `--since 2` prints only the last two. For `--follow`: run `& $ex watch "claude@conductor" --repo $d --follow` in one terminal, append another JSON line to `$f` from another, see it appear within ~250 ms; Ctrl-C exits cleanly. Then delete `$d`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -F <msg-file>   # "feat(supervise): ensemble watch <member> — tail a member stream feed (S0)"
```

---

### Task 6: Verify + double-gate land

- [ ] **Step 1: Full lib test sweep + clippy**

Run: `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib && CARGO_TARGET_DIR=$HOME/ensemble-target cargo clippy --all-targets -- -D warnings'`
Expected: all tests pass (prior 187 + new ndjson/supervise); clippy clean.

- [ ] **Step 2: Update the backlog**

In `docs/AUTONOMOUS-BACKLOG.md`, log the S0 landing under item 0.5 (stream primitive + `watch` done; S1 control plane next). `git add docs/AUTONOMOUS-BACKLOG.md`.

- [ ] **Step 3: Double-gate**

On the slice branch, build a self-contained gate prompt embedding `git diff main...HEAD`. Run codex from the EMPTY temp cwd `/c/Users/<you>/AppData/Local/Temp/ens_gate_cwd` (`codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -C <dir>`, prompt via stdin) and claude (`claude -p` via stdin). Verify post-gate that the gate cwd is still empty (codex didn't mutate the repo). LAND (ff-merge to main) ONLY after BOTH end `VERDICT: LGTM`. Bar for this slice: the feed's locking/torn-tail/cursor correctness under concurrency, member-path traversal confinement, and forward-compat rendering (unknown event kinds not hidden).

- [ ] **Step 4: Land + push + clean**

ff-merge to main, push with `<you> <you@example.com>`, delete the slice branch, clean any gate scratch.

---

## Self-Review

- **Spec coverage (S0 slice of §5/§7/§12):** `ndjson::Feed` (Task 1 = spec §5.1), `StreamEvent` schema + render (Task 2 = §5.2), member-path safety (Task 3 = §5.4), `ensemble watch` local tail (Tasks 4–5 = §7). `ControlCmd` (§5.3) is deliberately DEFERRED to S1 (no consumer in S0 — TDD forbids speculative code); noted here so it isn't mistaken for a gap.
- **Placeholder scan:** none — every code step carries complete code; `<msg-file>` denotes the `git commit -F` message file (backticks/`<...>` in messages must not go through `-m`).
- **Type consistency:** `Feed` (open/append/read_since/len/is_empty/path), `StreamEvent` variants + `render_event`/`render_line`, `member_stream_path`, `WatchArgs`/`parse_watch_args` are referenced identically across tasks and the `main.rs` shell. lib re-exports match the call sites (`ensemble::Feed`, `ensemble::member_stream_path`, `ensemble::parse_watch_args`, `ensemble::render_line`).
- **Scope:** one coherent slice (file-plane read side + tail); no backend, no control plane, no cross-machine — all sequenced to later slices.
