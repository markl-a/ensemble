use crate::adapter::Adapter;
use crate::blackboard::Blackboard;
use crate::crew::{CrewConfig, OnFlake};
use crate::gate::{decide, GateDecision, RoleVerdict};
use crate::verdict::parse_verdict;
use std::collections::HashMap;
use std::path::Path;

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
}

pub struct Conductor {
    crew: CrewConfig,
    adapters: HashMap<String, Box<dyn Adapter>>,
}

impl Conductor {
    pub fn new(crew: CrewConfig, adapters: HashMap<String, Box<dyn Adapter>>) -> Self {
        Self { crew, adapters }
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

        for round in 0..max {
            // 1) implementer
            let impl_role = self.crew.implementer_role();
            let impl_prompt = build_prompt(task, &bb, &feedback, impl_role);
            match self
                .adapter_for_role(impl_role)
                .map(|a| a.run(&impl_prompt, cwd))
            {
                Some(Ok(out)) => bb.post(&out.agent, "result", &out.text),
                Some(Err(e)) => {
                    return RunOutcome {
                        decision: Decision::Escalated(format!(
                            "implementer '{impl_role}' failed: {e}"
                        )),
                        rounds: round + 1,
                        blackboard: bb,
                    };
                }
                None => {
                    return RunOutcome {
                        decision: Decision::Escalated(format!(
                            "no adapter for implementer role '{impl_role}'"
                        )),
                        rounds: round + 1,
                        blackboard: bb,
                    };
                }
            }

            // 2) reviewers — a flaked reviewer is EXCLUDED (logged), never counted as approval.
            let mut verdicts: Vec<RoleVerdict> = Vec::new();
            for role in self.crew.reviewer_roles() {
                let prompt = build_prompt(task, &bb, &feedback, role);
                match self.adapter_for_role(role).map(|a| a.run(&prompt, cwd)) {
                    Some(Ok(out)) => {
                        let v = parse_verdict(&out.text);
                        bb.post(&out.agent, "verdict", &out.text);
                        verdicts.push(RoleVerdict {
                            role: role.to_string(),
                            agent: out.agent,
                            verdict: v,
                        });
                    }
                    Some(Err(e)) => {
                        // OnFlake::Exclude (the only Phase-1 policy): drop from quorum, log why.
                        let _ = OnFlake::Exclude;
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
                    }
                }
                GateDecision::Escalate(why) => {
                    return RunOutcome {
                        decision: Decision::Escalated(why),
                        rounds: round + 1,
                        blackboard: bb,
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
        }
    }

    /// Run the pipeline for `task` in a fresh git worktree of `repo`, cleaning it up afterward.
    /// Falls back to running in `repo` itself if the worktree can't be created (logged in the
    /// outcome).
    pub fn run_in_repo(&self, task: &str, repo: &Path) -> RunOutcome {
        match crate::worktree::Worktree::create(repo, task) {
            Ok(wt) => {
                self.run(task, &wt.path)
                // wt drops here → cleanup
            }
            Err(e) => {
                let mut out = self.run(task, repo);
                if let Decision::Landed = out.decision {
                    // surface the isolation failure without masking a real land/escalate
                    out.blackboard.post(
                        "ensemble",
                        "finding",
                        &format!("worktree unavailable, ran in repo root: {e}"),
                    );
                }
                out
            }
        }
    }
}

/// Build an agent's prompt: the task, the inter-agent blackboard summary, and any change-requests
/// routed back to the implementer this round.
fn build_prompt(task: &str, bb: &Blackboard, feedback: &[String], role: &str) -> String {
    let mut p = format!("You are the '{role}' role on a collaborative dev crew.\nTask: {task}\n");
    let summary = bb.summary();
    if !summary.is_empty() {
        p.push('\n');
        p.push_str(&summary);
    }
    if !feedback.is_empty() {
        p.push_str("\nReviewers requested these changes:\n");
        for f in feedback {
            p.push_str(&format!("- {f}\n"));
        }
    }
    p
}
