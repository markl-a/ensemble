//! Durable, resumable dispatch (Phase 3b-2): enqueue tasks into a Ledger, recover any stale claims,
//! then drain by claiming each and running it through the Conductor — writing a terminal record per
//! task. Re-running resumes: done tasks are skipped (already terminal), a crashed run's orphaned
//! claim is recovered first. The Conductor can't tell it's driven by a ledger vs `run_many`.

use crate::conductor::{Conductor, Decision};
use crate::ledger::{Counts, Ledger, Result};
use std::path::Path;

/// A stable id for a task's text, so re-running the same batch is idempotent (same id → INSERT OR
/// IGNORE no-ops). Content-stable AND toolchain-independent: FNV-1a 64-bit is a FIXED algorithm, so
/// the id stays the same across Rust upgrades. `DefaultHasher` (the previous impl) was only "stable
/// within a binary build" — its seed/algorithm can change between toolchains, which would re-id
/// every existing ledger task on a compiler upgrade and silently re-run a whole drained batch.
pub fn task_id(descr: &str) -> String {
    // FNV-1a 64-bit over the raw UTF-8 bytes. Same fixed constants as the phantom-mesh fleet's
    // task_id, so the durable PRIMARY KEY can never drift on a toolchain bump.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in descr.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
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

#[cfg(test)]
mod tests {
    use super::task_id;

    #[test]
    fn task_id_is_content_stable_and_toolchain_independent() {
        // GOLDEN: locks the FNV-1a output so an accidental algorithm change — or a regression
        // back to DefaultHasher — is caught. The durable ledger PRIMARY KEY depends on this id
        // never drifting across Rust toolchain upgrades (else a drained batch silently re-runs).
        assert_eq!(task_id("hello"), "a430d84680aabd0b");
        assert_eq!(task_id("world"), "4f59ff5e730c8af3");
        assert_eq!(task_id(""), "cbf29ce484222325", "empty input = the FNV offset basis");
        // Deterministic + distinct.
        assert_eq!(task_id("hello"), task_id("hello"));
        assert_ne!(task_id("hello"), task_id("world"));
        // Always 16 lowercase hex chars (zero-padded).
        let id = task_id("deploy the thing");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
