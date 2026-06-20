use crate::adapter::Adapter;
use crate::blackboard::Blackboard;
use crate::crew::{CrewConfig, OnFlake};
use crate::gate::{decide, GateDecision, RoleVerdict};
use crate::verdict::parse_verdict;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug)]
pub enum Decision {
    Landed,
    Escalated(String),
}

#[derive(Debug)]
pub struct RunOutcome {
    pub decision: Decision,
    pub rounds: u32,
    pub blackboard: Blackboard,
    /// On a LANDED `run_in_repo`, the `ensemble/<slug>` branch the committed work was kept on (so
    /// the operator can merge it). `None` for escalated runs and for `run()` (no worktree).
    pub branch: Option<String>,
}

pub struct Conductor {
    crew: CrewConfig,
    adapters: HashMap<String, Box<dyn Adapter>>,
    /// Firewall B: a Ctrl-C handler flips this; the conductor bails cleanly at the next round
    /// boundary. Defaults to a never-set flag (so a plain `Conductor::new` is unaffected).
    abort: Arc<AtomicBool>,
}

/// Firewall B: true when `elapsed_secs` has exceeded a wall-clock budget (`budget == 0` ⇒ no budget).
fn over_budget(elapsed_secs: u64, budget: u64) -> bool {
    budget > 0 && elapsed_secs >= budget
}

impl Conductor {
    pub fn new(crew: CrewConfig, adapters: HashMap<String, Box<dyn Adapter>>) -> Self {
        Self {
            crew,
            adapters,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Wire an external abort flag (set by a Ctrl-C handler) so a run stops cleanly at the next
    /// round boundary (firewall B).
    pub fn with_abort(mut self, flag: Arc<AtomicBool>) -> Self {
        self.abort = flag;
        self
    }

    fn adapter_for_role(&self, role: &str) -> Option<&dyn Adapter> {
        let agent = &self.crew.roles.get(role)?.agent;
        self.adapters.get(agent).map(|b| b.as_ref())
    }

    /// Run the role pipeline on `task` until the gate lands it, escalates, or rounds run out.
    pub fn run(&self, task: &str, cwd: &Path) -> RunOutcome {
        let mut bb = Blackboard::new();
        let mut feedback: Vec<String> = Vec::new();
        let max = self.crew.gate.max_rounds.max(1);
        let started = Instant::now();
        let mut last_sig: Option<String> = None; // firewall B: previous round's progress signature
        let mut same = 0u32; // consecutive identical-signature rounds

        for round in 0..max {
            // ── Firewall B: abort (Ctrl-C) + wall-clock budget, checked at each round boundary ──
            if self.abort.load(Ordering::Relaxed) {
                return RunOutcome {
                    decision: Decision::Escalated("aborted by operator".to_string()),
                    rounds: round,
                    blackboard: bb,
                    branch: None,
                };
            }
            if over_budget(started.elapsed().as_secs(), self.crew.gate.max_task_secs) {
                return RunOutcome {
                    decision: Decision::Escalated(format!(
                        "timed out after {}s",
                        self.crew.gate.max_task_secs
                    )),
                    rounds: round,
                    blackboard: bb,
                    branch: None,
                };
            }

            // 1) implementer
            let impl_role = self.crew.implementer_role();
            let impl_prompt = build_prompt(task, &bb, &feedback, impl_role, false);
            let impl_text;
            match self
                .adapter_for_role(impl_role)
                .map(|a| a.run(&impl_prompt, cwd))
            {
                Some(Ok(out)) => {
                    impl_text = out.text.clone();
                    bb.post(&out.agent, "result", &out.text);
                }
                Some(Err(e)) => {
                    return RunOutcome {
                        decision: Decision::Escalated(format!(
                            "implementer '{impl_role}' failed: {e}"
                        )),
                        rounds: round + 1,
                        blackboard: bb,
                        branch: None,
                    };
                }
                None => {
                    return RunOutcome {
                        decision: Decision::Escalated(format!(
                            "no adapter for implementer role '{impl_role}'"
                        )),
                        rounds: round + 1,
                        blackboard: bb,
                        branch: None,
                    };
                }
            }

            // ── TEST GATE (firewall A) ── the project's real tests must be GREEN before the AI
            // reviewers run. RED bounces the traceback back to the implementer (don't spend reviewer
            // turns on code that doesn't pass); a suite that never goes green can NEVER land.
            let mut test_passed = true;
            let mut test_out = String::new();
            if let Some(test) = &self.crew.test {
                let t = crate::test_gate::run_tests(cwd, &test.command);
                test_passed = t.passed;
                test_out = t.output.clone();
                bb.post(
                    "test",
                    if t.passed {
                        "test_pass"
                    } else {
                        "test_failure"
                    },
                    &t.output,
                );
            }

            // ── CIRCUIT BREAKER (firewall B) ── break early on NO PROGRESS: the implementer's output
            // and the test result are byte-identical to the previous round (it's spinning). Trips
            // before grinding to `max_rounds`. Repeated identical test failures are the sharpest
            // signal — so this sits before the red-bounce below.
            let sig = format!("{impl_text}\u{1}{test_out}");
            if last_sig.as_deref() == Some(sig.as_str()) {
                same += 1;
            } else {
                same = 1;
                last_sig = Some(sig);
            }
            if self.crew.gate.stall_limit > 0 && same >= self.crew.gate.stall_limit {
                return RunOutcome {
                    decision: Decision::Escalated(format!(
                        "circuit-broken: no progress across {same} identical rounds"
                    )),
                    rounds: round + 1,
                    blackboard: bb,
                    branch: None,
                };
            }

            // test RED → bounce the traceback back to the implementer, skip reviewers this round; a
            // suite that never goes green can NEVER land.
            if !test_passed {
                if round + 1 >= max {
                    return RunOutcome {
                        decision: Decision::Escalated(format!(
                            "tests never passed after {} round(s)",
                            round + 1
                        )),
                        rounds: round + 1,
                        blackboard: bb,
                        branch: None,
                    };
                }
                feedback = vec![format!(
                    "Your changes did not pass the test suite. Fix WITHOUT breaking existing \
                     behaviour. Test output:\n{}",
                    test_out
                )];
                continue;
            }

            // 2) reviewers — a flaked reviewer is EXCLUDED (logged), never counted as approval.
            // When the primary adapter errors, consult `on_flake`: Retry re-runs the same agent
            // once; Substitute falls back to the agent's configured backup. A verdict only enters
            // the quorum from a real `Some(Ok(..))` — a flake is never faked into an approval.
            let mut verdicts: Vec<RoleVerdict> = Vec::new();
            for role in self.crew.reviewer_roles() {
                let prompt = build_prompt(task, &bb, &feedback, role, true);
                let agent_name = self
                    .crew
                    .roles
                    .get(role)
                    .map(|r| r.agent.clone())
                    .unwrap_or_default();
                let mut result = self.adapter_for_role(role).map(|a| a.run(&prompt, cwd));
                let mut effective_agent = agent_name.clone();

                if matches!(result, Some(Err(_))) {
                    match self.crew.gate.on_flake {
                        OnFlake::Exclude => {}
                        OnFlake::Retry => {
                            bb.post(role, "finding", "reviewer flaked — retrying once");
                            result = self.adapter_for_role(role).map(|a| a.run(&prompt, cwd));
                        }
                        OnFlake::Substitute => {
                            if let Some(backup) = self.crew.backup_for(&agent_name) {
                                if let Some(b) = self.adapters.get(backup) {
                                    bb.post(
                                        role,
                                        "finding",
                                        &format!(
                                            "reviewer flaked — substituting backup '{backup}'"
                                        ),
                                    );
                                    effective_agent = backup.to_string();
                                    result = Some(b.run(&prompt, cwd));
                                }
                            }
                        }
                    }
                }

                match result {
                    Some(Ok(out)) => {
                        let v = parse_verdict(&out.text);
                        bb.post(&out.agent, "verdict", &out.text);
                        verdicts.push(RoleVerdict {
                            role: role.to_string(),
                            agent: effective_agent,
                            verdict: v,
                        });
                    }
                    Some(Err(e)) => {
                        bb.post(role, "finding", &format!("reviewer excluded — flaked: {e}"));
                    }
                    None => {
                        bb.post(
                            role,
                            "finding",
                            &format!("reviewer excluded — no adapter for role '{role}'"),
                        );
                    }
                }
            }

            // 3) gate
            match decide(&verdicts, &self.crew.gate, round) {
                GateDecision::Land => {
                    return RunOutcome {
                        decision: Decision::Landed,
                        rounds: round + 1,
                        blackboard: bb,
                        branch: None,
                    }
                }
                GateDecision::Escalate(why) => {
                    return RunOutcome {
                        decision: Decision::Escalated(why),
                        rounds: round + 1,
                        blackboard: bb,
                        branch: None,
                    }
                }
                GateDecision::Iterate(changes) => {
                    feedback = changes;
                }
            }
        }
        RunOutcome {
            decision: Decision::Escalated("max rounds reached".to_string()),
            rounds: max,
            blackboard: bb,
            branch: None,
        }
    }

    /// Run the pipeline for `task` in a fresh git worktree of `repo`, cleaning it up afterward.
    /// If the worktree can't be created, ESCALATE — never fall back to running in `repo` itself:
    /// under `run_many` another task may be mutating the shared working tree concurrently, so a
    /// task that can't get an isolated worktree must not run at all (isolation is the contract).
    pub fn run_in_repo(&self, task: &str, repo: &Path) -> RunOutcome {
        match crate::worktree::Worktree::create(repo, task) {
            Ok(mut wt) => {
                let mut out = self.run(task, &wt.path);
                if matches!(out.decision, Decision::Landed) {
                    // Persist: capture the agents' edits and keep the branch so the work survives.
                    match wt.commit(&format!("ensemble: {task}")) {
                        Ok(_) => {
                            wt.keep();
                            out.branch = Some(wt.branch().to_string());
                        }
                        Err(e) => {
                            // Persisting failed → the branch is deleted on Drop, so the work is
                            // GONE. Never report a clean LAND for lost work: downgrade to Escalated
                            // so the CLI headline, the process exit code, and `run_many`'s
                            // any-escalated check all reflect that the operator must intervene (the
                            // transcript still records what the agents produced).
                            out.blackboard.post(
                                "ensemble",
                                "finding",
                                &format!("commit failed, work NOT persisted: {e}"),
                            );
                            out.decision = Decision::Escalated(format!(
                                "commit failed, work not persisted: {e}"
                            ));
                        }
                    }
                }
                out // wt drops → worktree removed; branch kept iff we called keep()
            }
            Err(e) => {
                let mut bb = Blackboard::new();
                bb.post(
                    "ensemble",
                    "finding",
                    &format!(
                        "worktree unavailable — task not run (is `{}` a git repo?): {e}",
                        repo.display()
                    ),
                );
                RunOutcome {
                    decision: Decision::Escalated(format!("worktree unavailable: {e}")),
                    rounds: 0,
                    blackboard: bb,
                    branch: None,
                }
            }
        }
    }

    /// Run many tasks in parallel — each in its own git worktree of `repo`. Results are returned in
    /// the same order as `tasks`. (甲) For Phase 2 we spawn one thread per task (the task list is
    /// operator-sized, not unbounded); a bounded pool is a later refinement.
    pub fn run_many(&self, tasks: &[String], repo: &Path) -> Vec<RunOutcome> {
        use std::sync::Mutex;
        let results: Vec<Mutex<Option<RunOutcome>>> =
            (0..tasks.len()).map(|_| Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for (i, task) in tasks.iter().enumerate() {
                let slot = &results[i];
                s.spawn(move || {
                    let out = self.run_in_repo(task, repo);
                    *slot.lock().unwrap() = Some(out);
                });
            }
        });
        results
            .into_iter()
            .map(|m| m.into_inner().unwrap().unwrap())
            .collect()
    }
}

/// Build an agent's prompt: the task, the inter-agent blackboard summary, and any change-requests
/// routed back to the implementer this round.
fn build_prompt(
    task: &str,
    bb: &Blackboard,
    feedback: &[String],
    role: &str,
    is_reviewer: bool,
) -> String {
    let _ = role;
    let summary = bb.summary();
    if is_reviewer {
        // Reviewer: judge the IMPLEMENTER's work — do NOT redo the task — and end with a parseable
        // VERDICT line (else `parse_verdict` conservatively reads it as changes-requested and the
        // task can never land).
        let mut p = format!(
            "You are a REVIEWER on a dev crew. A teammate (the implementer) was asked to do:\n\
             TASK: {task}\n\n"
        );
        if !summary.is_empty() {
            p.push_str("Activity so far (the implementer's output is the `result` entry):\n");
            p.push_str(&summary);
            p.push('\n');
        }
        p.push_str(
            "Judge ONLY whether the implementer's work satisfies the task. Do NOT do the task \
             yourself. Give a one-line reason, then end with EXACTLY one final line:\n\
             VERDICT: LGTM                    (if it satisfies the task)\n\
             VERDICT: CHANGES: <what to fix>  (otherwise)\n",
        );
        p
    } else {
        // Implementer: do the task now and produce the deliverable.
        let mut p = format!(
            "You are the IMPLEMENTER on a dev crew. Do this task now and produce the deliverable:\n\
             TASK: {task}\n"
        );
        if !feedback.is_empty() {
            p.push_str("\nA reviewer asked you to fix:\n");
            for f in feedback {
                p.push_str(&format!("- {f}\n"));
            }
        }
        if !summary.is_empty() {
            p.push('\n');
            p.push_str(&summary);
            p.push('\n');
        }
        p.push_str("\nAfter doing it, briefly state what you produced.\n");
        p
    }
}

#[cfg(test)]
mod tests {
    use super::over_budget;

    #[test]
    fn over_budget_respects_a_zero_disabled_budget() {
        assert!(!over_budget(0, 0)); // 0 budget = disabled
        assert!(!over_budget(9_999, 0)); // disabled even at large elapsed
        assert!(!over_budget(2, 3)); // under budget
        assert!(over_budget(3, 3)); // at budget
        assert!(over_budget(5, 3)); // over budget
    }
}
