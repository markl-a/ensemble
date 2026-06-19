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

impl Task {
    pub fn state_str(&self) -> &'static str {
        self.state.as_str()
    }
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
        // Install the busy timeout FIRST so the WAL pragma + schema below WAIT for a concurrent
        // writer/initializer instead of failing fast with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
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
    ///
    /// NOTE: this gives at-most-once CLAIM but at-LEAST-once EXECUTION — if a worker finishes the
    /// work (e.g. lands a git branch) and dies BEFORE this terminal write, `recover_orphans` will
    /// requeue and re-run it. Side effects must therefore be idempotent for re-runs to be safe (the
    /// conductor's worktree-per-run uses a fresh unique branch, so a re-run produces a NEW branch
    /// rather than colliding). A retry/exactly-once policy is a Phase-3b-2b concern.
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
        assert!(
            !l.enqueue("a", "task a", 2).unwrap(),
            "dup id must be ignored"
        );
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
        Ledger::open(&path)
            .unwrap()
            .enqueue("solo", "one", 1)
            .unwrap();
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
