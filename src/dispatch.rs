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
/// `conductor`, recording a terminal record per task. `clock` is called FRESH for every claim and
/// terminal write — a task claimed late in a long batch must stamp its real claim time, or a
/// concurrent `recover_orphans` (cutoff = start − 5min) would requeue and re-run a live task.
/// `stale_before` is the one-shot recovery cutoff. Both are injected for testing.
pub fn run(
    ledger: &mut Ledger,
    conductor: &Conductor,
    tasks: &[String],
    repo: &Path,
    worker: &str,
    clock: &dyn Fn() -> i64,
    stale_before: i64,
) -> Result<Counts> {
    for t in tasks {
        ledger.enqueue(&task_id(t), t, clock())?;
    }
    ledger.recover_orphans(stale_before)?;
    while !conductor.aborted() {
        let task = match ledger.claim(worker, clock())? {
            Some(t) => t,
            None => break, // queue drained
        };
        let out = conductor.run_in_repo(&task.descr, repo);
        // Operator abort (firewall B): the conductor aborted this run. Leave the claim as-is so
        // `recover_orphans` requeues it later, and stop claiming new work — do NOT fail-mark the
        // rest of the queue.
        if conductor.aborted() {
            break;
        }
        match out.decision {
            Decision::Landed => {
                let branch = out.branch.as_deref().unwrap_or("");
                let outcome = format!("LANDED {branch}");
                ledger.complete(&task.id, outcome.trim_end(), clock())?;
            }
            Decision::Escalated(why) => {
                ledger.fail(&task.id, &format!("ESCALATED: {why}"), clock())?;
            }
        }
    }
    ledger.counts()
}
