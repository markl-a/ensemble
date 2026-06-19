# ensemble Phase-3b-2 — SQLite coordination ledger (durable, resumable dispatch)

> **For agentic workers:** TDD per task. **Build/test via WSL** (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`) — native debug hits Defender LNK1104. Work in `D:\Projects\ensemble` on `main`. Gate every change with codex+claude.

**Goal:** today dispatch is synchronous + in-memory — if the orchestrator dies mid-run, what completed is lost and nothing prevents re-running a finished task. Add a **durable SQLite (WAL) coordination ledger** so a batch of tasks survives a crash: tasks are enqueued at-most-once, **claimed atomically** (one task → one worker), a task is "done" ONLY when a terminal record is written (yonder: silence ≠ success), and a dead worker's **orphaned claim is recovered** back to the queue. Wire it as a real, resumable `ensemble dispatch` command.

**Architecture:** new `src/ledger.rs` = the SQLite data layer (`rusqlite`, bundled). New `src/dispatch.rs` = the glue: enqueue → recover stale claims → drain (claim → `Conductor::run_in_repo` → terminal record) → counts; re-running resumes (done tasks skipped, orphans recovered). CLI gains `ensemble dispatch` + `ensemble ledger status|recover`.

**Slice-1 scope:** single-process durable dispatch against a LOCAL ledger file. The cross-machine shared ledger (exposed over HTTP / shared FS so remote `serve` workers pull from it) + a node registry/heartbeat table = **Phase 3b-2b** (honest follow-up; the claim/recover primitives are built + tested ready for it). Timestamps are injected as `i64` params (testable; production passes `SystemTime::now`).

**Tech:** `rusqlite = { version = "0.31", features = ["bundled"] }` (bundles SQLite C — compiles under WSL gcc / native MSVC; no system sqlite needed).

---

### Task 1: `rusqlite` dependency

**Files:** `Cargo.toml`.
- [ ] Add to `[dependencies]`: `rusqlite = { version = "0.31", features = ["bundled"] }`.
- [ ] `cargo build` (WSL) succeeds (first build compiles bundled sqlite — slow, ok). Commit `chore(phase3b2): add rusqlite (bundled) for the coordination ledger`.

---

### Task 2: `src/ledger.rs` — the SQLite coordination ledger

**Files:** Create `src/ledger.rs`; modify `src/lib.rs` (`pub mod ledger;` + re-export `Ledger, Task, TaskState, Counts, LedgerError`).

- [ ] **Step 1 (impl):** the module (see full code below — `open` with WAL + busy_timeout + migration; `enqueue` idempotent via `INSERT OR IGNORE`; `claim` atomic via an IMMEDIATE transaction; `complete`/`fail` terminal records; `recover_orphans`; `counts`/`list`).
- [ ] **Step 2 (test):** six hermetic tests (temp DB), including an 8-thread at-most-once concurrency test.
- [ ] **Step 3:** `cargo test` green; fmt; clippy. Commit `feat(phase3b2): ledger — durable WAL task store (enqueue/claim/complete/recover)`.

Full module:
```rust
//! src/ledger.rs — a durable SQLite coordination ledger (Phase 3b-2). Makes a batch of dispatched
//! tasks survive a crash: tasks live in a WAL'd SQLite file, are claimed AT-MOST-ONCE, and a task is
//! "done" only when a terminal record is written (yonder: silence ≠ success). A worker that dies
//! leaves a stale claim that `recover_orphans` returns to the queue. Grounded in design §4b(b/c).

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
pub type Result<T> = std::result::Result<T, LedgerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Claimed,
    Done,
    Failed,
}
impl TaskState {
    fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Claimed => "claimed",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
        }
    }
    fn parse(s: &str) -> TaskState {
        match s {
            "claimed" => TaskState::Claimed,
            "done" => TaskState::Done,
            "failed" => TaskState::Failed,
            _ => TaskState::Queued,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub descr: String,
    pub state: TaskState,
    pub claimed_by: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub queued: usize,
    pub claimed: usize,
    pub done: usize,
    pub failed: usize,
}

/// A SQLite-backed coordination ledger. One per worker/process; many may point at the same file.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id           TEXT PRIMARY KEY,
                descr        TEXT NOT NULL,
                state        TEXT NOT NULL,
                created      INTEGER NOT NULL,
                claimed_by   TEXT,
                claimed_at   INTEGER,
                completed_at INTEGER,
                outcome      TEXT
            );",
        )?;
        Ok(Self { conn })
    }

    /// Enqueue a task. Idempotent — a duplicate id is ignored (at-most-once enqueue, so re-running a
    /// batch never double-creates). Returns true if newly inserted.
    pub fn enqueue(&self, id: &str, descr: &str, now: i64) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO tasks (id, descr, state, created) VALUES (?, ?, 'queued', ?)",
            params![id, descr, now],
        )?;
        Ok(n == 1)
    }

    /// Atomically claim the oldest queued task for `worker`, or None if the queue is empty. The
    /// IMMEDIATE transaction takes the write lock up front, so concurrent claimers serialize and a
    /// task is handed to EXACTLY ONE worker.
    pub fn claim(&mut self, worker: &str, now: i64) -> Result<Option<Task>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT id, descr FROM tasks WHERE state='queued' ORDER BY created, id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let task = if let Some((id, descr)) = row {
            tx.execute(
                "UPDATE tasks SET state='claimed', claimed_by=?, claimed_at=? WHERE id=?",
                params![worker, now, id],
            )?;
            Some(Task {
                id,
                descr,
                state: TaskState::Claimed,
                claimed_by: Some(worker.to_string()),
                outcome: None,
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(task)
    }

    /// Write the terminal record: the task is DONE (the only success signal).
    pub fn complete(&self, id: &str, outcome: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET state='done', outcome=?, completed_at=? WHERE id=?",
            params![outcome, now, id],
        )?;
        Ok(())
    }

    /// Terminal record for a failed task.
    pub fn fail(&self, id: &str, reason: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET state='failed', outcome=?, completed_at=? WHERE id=?",
            params![reason, now, id],
        )?;
        Ok(())
    }

    /// Return claims older than `stale_before` to the queue (a dead worker's orphaned claims).
    /// Returns how many were recovered.
    pub fn recover_orphans(&self, stale_before: i64) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE tasks SET state='queued', claimed_by=NULL, claimed_at=NULL \
             WHERE state='claimed' AND claimed_at < ?",
            params![stale_before],
        )?;
        Ok(n)
    }

    pub fn counts(&self) -> Result<Counts> {
        let mut c = Counts::default();
        let mut stmt = self
            .conn
            .prepare("SELECT state, COUNT(*) FROM tasks GROUP BY state")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })?;
        for row in rows {
            let (s, n) = row?;
            match TaskState::parse(&s) {
                TaskState::Queued => c.queued = n,
                TaskState::Claimed => c.claimed = n,
                TaskState::Done => c.done = n,
                TaskState::Failed => c.failed = n,
            }
        }
        Ok(c)
    }

    pub fn list(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, descr, state, claimed_by, outcome FROM tasks ORDER BY created, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Task {
                id: r.get(0)?,
                descr: r.get(1)?,
                state: TaskState::parse(&r.get::<_, String>(2)?),
                claimed_by: r.get(3)?,
                outcome: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

// keep `as_str` used (state writes are literals above; expose for callers/tests)
impl Task {
    pub fn state_str(&self) -> &'static str {
        self.state.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_tmp() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open(&dir.path().join("ledger.db")).unwrap();
        (dir, l)
    }

    #[test]
    fn enqueue_is_idempotent() {
        let (_d, l) = open_tmp();
        assert!(l.enqueue("a", "task a", 1).unwrap());
        assert!(!l.enqueue("a", "task a", 2).unwrap(), "dup id must be ignored");
        assert_eq!(l.counts().unwrap().queued, 1);
    }

    #[test]
    fn claim_drains_queue_then_none() {
        let (_d, mut l) = open_tmp();
        l.enqueue("a", "A", 1).unwrap();
        l.enqueue("b", "B", 2).unwrap();
        assert_eq!(l.claim("w", 10).unwrap().unwrap().id, "a");
        assert_eq!(l.claim("w", 10).unwrap().unwrap().id, "b");
        assert!(l.claim("w", 10).unwrap().is_none());
        assert_eq!(l.counts().unwrap().claimed, 2);
    }

    #[test]
    fn claim_is_at_most_once_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        Ledger::open(&path).unwrap().enqueue("solo", "one", 1).unwrap();
        let got = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|s| {
            for i in 0..8 {
                let path = path.clone();
                let got = got.clone();
                s.spawn(move || {
                    let mut l = Ledger::open(&path).unwrap();
                    if l.claim(&format!("w{i}"), 10).unwrap().is_some() {
                        got.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(
            got.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "exactly one worker may claim a task"
        );
    }

    #[test]
    fn complete_and_fail_write_terminal_state() {
        let (_d, mut l) = open_tmp();
        l.enqueue("a", "A", 1).unwrap();
        l.enqueue("b", "B", 1).unwrap();
        l.claim("w", 10).unwrap();
        l.claim("w", 10).unwrap();
        l.complete("a", "LANDED ensemble/a", 20).unwrap();
        l.fail("b", "ESCALATED: boom", 20).unwrap();
        let c = l.counts().unwrap();
        assert_eq!((c.done, c.failed), (1, 1));
        let a = l.list().unwrap().into_iter().find(|t| t.id == "a").unwrap();
        assert_eq!(a.outcome.as_deref(), Some("LANDED ensemble/a"));
    }

    #[test]
    fn recover_orphans_requeues_stale_claims() {
        let (_d, mut l) = open_tmp();
        l.enqueue("a", "A", 1).unwrap();
        l.claim("dead-worker", 5).unwrap(); // claimed_at = 5
        // recover anything claimed before t=100 → our claim (5) is stale
        assert_eq!(l.recover_orphans(100).unwrap(), 1);
        assert_eq!(l.counts().unwrap().queued, 1);
        // a FRESH claim (claimed_at = 200) is NOT recovered by an earlier cutoff
        l.claim("w", 200).unwrap();
        assert_eq!(l.recover_orphans(100).unwrap(), 0);
    }

    #[test]
    fn counts_reflect_states() {
        let (_d, mut l) = open_tmp();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            l.enqueue(id, id, i as i64).unwrap();
        }
        l.claim("w", 10).unwrap();
        l.complete("a", "ok", 20).unwrap(); // 'a' was the oldest → claimed → done
        let c = l.counts().unwrap();
        assert_eq!((c.queued, c.claimed, c.done, c.failed), (2, 0, 1, 0));
    }
}
```

---

### Task 3: `src/dispatch.rs` — durable, resumable drain glue

**Files:** Create `src/dispatch.rs`; modify `src/lib.rs` (`pub mod dispatch;` + re-export `dispatch::{run as dispatch_run, task_id}`).

- [ ] **Step 1 (impl):**
```rust
//! Durable, resumable dispatch (Phase 3b-2): enqueue tasks into a Ledger, recover any stale claims,
//! then drain by claiming each and running it through the Conductor — writing a terminal record per
//! task. Re-running resumes: done tasks are skipped (already terminal), a crashed run's orphaned
//! claim is recovered first. The Conductor can't tell it's driven by a ledger vs `run_many`.

use crate::conductor::{Conductor, Decision};
use crate::ledger::{Counts, Ledger, Result};
use std::path::Path;

/// A stable id for a task's text, so re-running the same batch is idempotent (same id → INSERT OR
/// IGNORE no-ops). Stable within a binary build.
pub fn task_id(descr: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    descr.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Enqueue `tasks`, recover claims older than `stale_before`, then drain the queue through
/// `conductor`, recording a terminal record per task. `now`/`stale_before` are injected (testable).
pub fn run(
    ledger: &mut Ledger,
    conductor: &Conductor,
    tasks: &[String],
    repo: &Path,
    worker: &str,
    now: i64,
    stale_before: i64,
) -> Result<Counts> {
    for t in tasks {
        ledger.enqueue(&task_id(t), t, now)?;
    }
    ledger.recover_orphans(stale_before)?;
    while let Some(task) = ledger.claim(worker, now)? {
        let out = conductor.run_in_repo(&task.descr, repo);
        match out.decision {
            Decision::Landed => {
                let branch = out.branch.as_deref().unwrap_or("");
                ledger.complete(&task.id, &format!("LANDED {branch}").trim_end(), now)?;
            }
            Decision::Escalated(why) => {
                ledger.fail(&task.id, &format!("ESCALATED: {why}"), now)?;
            }
        }
    }
    ledger.counts()
}
```
> NOTE: `&format!(...).trim_end()` returns `&str`; `complete` takes `&str` — fine. If clippy objects, bind to a `let s = ...;` first.

- [ ] **Step 2 (test):** `tests/ledger_dispatch.rs` — a real git repo + a Conductor of mock adapters (an implementer that writes a file + an always-LGTM reviewer) + a ledger; assert all tasks reach `done` with kept branches, AND that a pre-existing stale `claimed` task is recovered and completed (resumability).

```rust
use ensemble::*;
use ensemble::ledger::Ledger;
use std::collections::HashMap;

struct Writer { name: String, file: String }
impl Adapter for Writer {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        std::fs::write(cwd.join(&self.file), "X").unwrap();
        Ok(AgentOutput { agent: self.name.clone(), text: format!("wrote {}", self.file) })
    }
}
struct Lgtm { name: String }
impl Adapter for Lgtm {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, _cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        Ok(AgentOutput { agent: self.name.clone(), text: "VERDICT: LGTM".into() })
    }
}
fn crew() -> CrewConfig {
    CrewConfig::from_toml(r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#).unwrap()
}
fn git_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for a in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
    }
    std::fs::write(repo.join("seed"), "x").unwrap();
    for a in [&["add","."][..], &["commit","-q","-m","init"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
    }
    tmp
}
fn conductor() -> Conductor {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    m.insert("codex".into(), Box::new(Writer { name: "codex".into(), file: "out.txt".into() }));
    m.insert("claude".into(), Box::new(Lgtm { name: "claude".into() }));
    Conductor::new(crew(), m)
}

#[test]
fn dispatch_drains_all_tasks_to_done() {
    let tmp = git_repo();
    let mut ledger = Ledger::open(&tmp.path().join("ledger.db")).unwrap();
    let c = conductor();
    let tasks = vec!["task one".to_string(), "task two".to_string()];
    let counts = ensemble::dispatch::run(&mut ledger, &c, &tasks, tmp.path(), "w", 1000, 0).unwrap();
    assert_eq!((counts.done, counts.failed, counts.queued), (2, 0, 0));
}

#[test]
fn dispatch_is_idempotent_and_recovers_orphans() {
    let tmp = git_repo();
    let path = tmp.path().join("ledger.db");
    let c = conductor();
    let tasks = vec!["only task".to_string()];

    // first run completes the task
    {
        let mut l = Ledger::open(&path).unwrap();
        let counts = ensemble::dispatch::run(&mut l, &c, &tasks, tmp.path(), "w", 1000, 0).unwrap();
        assert_eq!(counts.done, 1);
    }
    // simulate a SECOND, crashed worker that claimed a NEW task but never finished it
    {
        let l = Ledger::open(&path).unwrap();
        l.enqueue("orphan", "left mid-flight", 2000).unwrap();
    }
    {
        let mut l = Ledger::open(&path).unwrap();
        l.claim("dead", 2000).unwrap(); // claims 'orphan', claimed_at = 2000, never completed
    }
    // re-run with now=5000, stale_before=4000 → orphan (claimed at 2000) recovered + completed;
    // the already-done task is NOT re-run (idempotent enqueue + it's terminal)
    {
        let mut l = Ledger::open(&path).unwrap();
        let counts = ensemble::dispatch::run(&mut l, &c, &tasks, tmp.path(), "w2", 5000, 4000).unwrap();
        assert_eq!(counts.done, 2, "orphan recovered + completed; original stays done");
        assert_eq!(counts.queued + counts.claimed, 0);
    }
}
```

- [ ] **Step 3:** `cargo test` green; fmt; clippy. Commit `feat(phase3b2): dispatch — durable resumable drain (enqueue→recover→claim→record)`.

---

### Task 4: CLI — `ensemble dispatch` + `ensemble ledger status|recover`

**Files:** `src/main.rs`.

- [ ] **Step 1 (impl):** add subcommands + a `now_secs()` helper. Update `USAGE`.
```rust
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `ensemble dispatch --ledger <p> [--crew <p>] [--repo <p>] "<t1>" ...` — durable, resumable batch.
fn dispatch_cmd(args: &[String]) {
    let tasks = positional_tasks(args);
    if tasks.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let ledger_path = parse_flag(args, "--ledger").unwrap_or_else(|| "ensemble-ledger.db".into());
    let crew = load_crew(args);
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let c = Conductor::new(crew.clone(), adapters_for(&crew));
    let mut ledger = ensemble::ledger::Ledger::open(Path::new(&ledger_path)).unwrap_or_else(|e| {
        eprintln!("ledger {ledger_path}: {e}");
        std::process::exit(1);
    });
    let now = now_secs();
    let worker = format!("worker-{}", std::process::id());
    // recover claims stale > 5 min (a previous worker that died mid-task)
    let counts = ensemble::dispatch::run(&mut ledger, &c, &tasks, Path::new(&repo), &worker, now, now - 300)
        .unwrap_or_else(|e| {
            eprintln!("dispatch: {e}");
            std::process::exit(1);
        });
    println!(
        "dispatch: {} done, {} failed, {} queued, {} claimed",
        counts.done, counts.failed, counts.queued, counts.claimed
    );
    if counts.failed > 0 {
        std::process::exit(1);
    }
}

/// `ensemble ledger status|recover --ledger <p> [--stale-secs N]`
fn ledger_cmd(args: &[String]) {
    let sub = args.get(2).map(|s| s.as_str());
    let ledger_path = parse_flag(args, "--ledger").unwrap_or_else(|| "ensemble-ledger.db".into());
    let l = ensemble::ledger::Ledger::open(Path::new(&ledger_path)).unwrap_or_else(|e| {
        eprintln!("ledger {ledger_path}: {e}");
        std::process::exit(1);
    });
    match sub {
        Some("status") => {
            let c = l.counts().unwrap_or_default();
            println!("queued={} claimed={} done={} failed={}", c.queued, c.claimed, c.done, c.failed);
            for t in l.list().unwrap_or_default() {
                let out = t.outcome.unwrap_or_default();
                println!("  [{}] {} — {}{}", t.state_str(), t.id, t.descr, if out.is_empty() { String::new() } else { format!(" ({out})") });
            }
        }
        Some("recover") => {
            let stale = parse_flag(args, "--stale-secs").and_then(|s| s.parse::<i64>().ok()).unwrap_or(300);
            let n = l.recover_orphans(now_secs() - stale).unwrap_or(0);
            println!("recovered {n} orphaned claim(s)");
        }
        _ => {
            eprintln!("usage: ensemble ledger <status|recover> --ledger <path> [--stale-secs N]");
            std::process::exit(2);
        }
    }
}
```
Wire into `main()`:
```rust
        Some("dispatch") => dispatch_cmd(&args),
        Some("ledger") => ledger_cmd(&args),
```
Extend `USAGE` with the two new lines. (`CrewConfig` must derive `Clone` — it likely does; if not, load the crew twice or add `#[derive(Clone)]`. Check `crew.rs`.)

- [ ] **Step 2:** `cargo build` + `cargo test` green; fmt; clippy `-D warnings`. Manual: `ensemble dispatch --ledger /tmp/l.db --repo <gitrepo> "x"` then `ensemble ledger status --ledger /tmp/l.db`. Commit `feat(phase3b2): CLI — ensemble dispatch + ledger status/recover`.

---

### Task 5: docs

**Files:** `docs/AUTONOMOUS-BACKLOG.md` (check off 3b-2, log), `docs/2026-06-19-ensemble-design.md` (3b-2 ✅ note + 3b-2b follow-up).
- [ ] Commit `docs(phase3b2): coordination ledger done — durable resumable dispatch`.

---

## Notes / deferred (3b-2b+)
- **Cross-machine shared ledger:** expose the ledger over HTTP (or a shared FS) so remote `serve` workers PULL claims from it — turns single-process dispatch into a true multi-node pull fleet. The claim/recover primitives are already built for this.
- **Node registry + heartbeat table:** `nodes(id, url, agents, last_seen)`; key orphan recovery off node liveness (last_seen) instead of claim age. Slice-1 uses claim-age, which is simpler and sufficient.
- **Crew config clone:** if `CrewConfig` isn't `Clone`, `dispatch_cmd` loads it once and `adapters_for` borrows it — adjust ordering rather than cloning.
- **Heartbeat-renewed leases (gate follow-up, codex+claude):** the 300s stale-claim cutoff is a fixed lease with no renewal, so a single conductor run that legitimately exceeds ~5min can be recovered by a later worker while still live. Made SAFE for now by the documented fresh-branch idempotency (a re-run produces a new branch, not a corrupt merge), but 3b-2b should add a heartbeat that renews `claimed_at` for a long-running claim.
- **State-guarded terminal writes (claude note #3):** `complete`/`fail` use `WHERE id=?` only — fine single-process; 3b-2b's concurrent completers should add `AND state='claimed'` + check `rows_changed == 1`.
- **Retry policy:** `fail` is terminal and `INSERT OR IGNORE` won't re-queue, so a transient `Escalated` is permanent across re-runs. A "retry failed" path is a later option.
