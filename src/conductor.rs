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
    /// S1a live supervision: when set, every blackboard post is mirrored here so `ensemble watch`
    /// can tail the run in real time. `None` ⇒ no streaming (unchanged behaviour).
    stream: Option<Box<dyn crate::supervise::RunObserver>>,
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
            stream: None,
        }
    }

    /// Wire an external abort flag (set by a Ctrl-C handler) so a run stops cleanly at the next
    /// round boundary (firewall B).
    pub fn with_abort(mut self, flag: Arc<AtomicBool>) -> Self {
        self.abort = flag;
        self
    }

    /// S1a: wire a live stream observer — every blackboard post is mirrored to it so the run is
    /// watchable in real time via `ensemble watch`. Best-effort; it never changes a run's outcome.
    pub fn with_stream(mut self, obs: Box<dyn crate::supervise::RunObserver>) -> Self {
        self.stream = Some(obs);
        self
    }

    /// Post to the blackboard AND mirror to the live stream observer (if any) — the single funnel so
    /// the run transcript and the live feed can never drift.
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

    /// Whether the operator has aborted (firewall B). A driver loop (e.g. `dispatch`) checks this to
    /// stop claiming new work rather than fail-marking the whole queue.
    pub fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
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
                    self.note(&mut bb, &out.agent, "result", &out.text);
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
                self.note(
                    &mut bb,
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
            // `.max(2)` so a misconfigured `stall_limit = 1` can't trip at round 0 (the first round
            // has nothing to compare against) — the minimum meaningful value is 2 identical rounds.
            if self.crew.gate.stall_limit > 0 && same >= self.crew.gate.stall_limit.max(2) {
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
                            self.note(&mut bb, role, "finding", "reviewer flaked — retrying once");
                            result = self.adapter_for_role(role).map(|a| a.run(&prompt, cwd));
                        }
                        OnFlake::Substitute => {
                            if let Some(backup) = self.crew.backup_for(&agent_name) {
                                if let Some(b) = self.adapters.get(backup) {
                                    self.note(
                                        &mut bb,
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
                        self.note(&mut bb, &out.agent, "verdict", &out.text);
                        verdicts.push(RoleVerdict {
                            role: role.to_string(),
                            agent: effective_agent,
                            verdict: v,
                        });
                    }
                    Some(Err(e)) => {
                        self.note(&mut bb, role, "finding", &format!("reviewer excluded — flaked: {e}"));
                    }
                    None => {
                        self.note(
                            &mut bb,
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
                    // Honor an operator abort that arrived mid-round: don't land work the operator
                    // asked to stop (firewall B). A budget overrun on a SUCCESSFUL round still lands
                    // — we don't discard completed work for the wall-clock net.
                    if self.abort.load(Ordering::Relaxed) {
                        return RunOutcome {
                            decision: Decision::Escalated("aborted by operator".to_string()),
                            rounds: round + 1,
                            blackboard: bb,
                            branch: None,
                        };
                    }
                    self.note(&mut bb, "conductor", "decision", "LANDED");
                    return RunOutcome {
                        decision: Decision::Landed,
                        rounds: round + 1,
                        blackboard: bb,
                        branch: None,
                    };
                }
                GateDecision::Escalate(why) => {
                    self.note(&mut bb, "conductor", "decision", &format!("escalated: {why}"));
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
        self.note(&mut bb, "conductor", "decision", "escalated: max rounds reached");
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
                // Per-run journal (design step 2): the blackboard transcript + the terminal decision,
                // written to `<repo>/.ensemble/runs/<slug>.jsonl` so the operator can replay what the
                // crew did. Best-effort — a journal write failure must never change the run's outcome.
                let (outcome, detail) = match &out.decision {
                    Decision::Landed => ("landed", out.branch.clone().unwrap_or_default()),
                    Decision::Escalated(why) => ("escalated", why.clone()),
                };
                let jsonl =
                    crate::journal::render(out.blackboard.read_since(0), outcome, &detail, out.rounds);
                let _ = crate::journal::write_run(repo, wt.slug(), &jsonl);
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

    #[test]
    fn run_mirrors_blackboard_posts_to_the_observer() {
        use super::*;
        use crate::adapter::{Adapter, MockAdapter};
        use crate::crew::{CrewConfig, GatePolicy, OnFlake, RoleConfig};
        use crate::supervise::RunObserver;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        // an observer that records (from, kind) of each mirrored post, behind an Arc so the test
        // can inspect it after the conductor has consumed the Box<dyn RunObserver>.
        struct Rec(Arc<Mutex<Vec<(String, String)>>>);
        impl RunObserver for Rec {
            fn post(&self, m: &crate::blackboard::Message) {
                self.0.lock().unwrap().push((m.from.clone(), m.kind.clone()));
            }
        }

        // a crew that lands in one round: impl -> one approving reviewer, min_approvals = 1
        let crew = CrewConfig {
            gate: GatePolicy {
                min_approvals: 1,
                max_rounds: 1,
                on_flake: OnFlake::Exclude,
                stall_limit: 0,
                max_task_secs: 0,
            },
            pipeline: vec!["implement".to_string(), "review".to_string()],
            roles: HashMap::from([
                ("implement".to_string(), RoleConfig { agent: "impl".to_string(), blind: false }),
                ("review".to_string(), RoleConfig { agent: "rev".to_string(), blind: false }),
            ]),
            agents: HashMap::new(),
            test: None,
        };
        let mut adapters: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        adapters.insert("impl".to_string(), Box::new(MockAdapter::new("impl", vec![Ok("implemented it".to_string())])));
        adapters.insert("rev".to_string(), Box::new(MockAdapter::new("rev", vec![Ok("VERDICT: LGTM".to_string())])));

        let log = Arc::new(Mutex::new(Vec::new()));
        let c = Conductor::new(crew, adapters).with_stream(Box::new(Rec(log.clone())));
        let out = c.run("do the thing", std::path::Path::new("."));
        assert!(matches!(out.decision, Decision::Landed), "should land: {:?}", out.decision);

        let seen = log.lock().unwrap().clone();
        assert!(seen.iter().any(|(f, k)| f == "impl" && k == "result"), "implementer result streamed: {seen:?}");
        assert!(seen.iter().any(|(f, k)| f == "rev" && k == "verdict"), "reviewer verdict streamed: {seen:?}");
        assert!(seen.iter().any(|(f, k)| f == "conductor" && k == "decision"), "terminal decision streamed: {seen:?}");
    }
}
