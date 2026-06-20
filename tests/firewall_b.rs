//! Firewall B: the circuit breaker (no-progress) + the operator abort flag.

use ensemble::*;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Always returns the same canned reply, ignoring the prompt — models an agent that makes no
/// progress round after round.
struct AlwaysOk {
    name: String,
    reply: String,
}
impl Adapter for AlwaysOk {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, _cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: self.reply.clone(),
        })
    }
}

fn crew_with_stall(stall_limit: u32, max_rounds: u32) -> CrewConfig {
    CrewConfig::from_toml(&format!(
        r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 2
        max_rounds = {max_rounds}
        on_flake = "exclude"
        stall_limit = {stall_limit}
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#
    ))
    .unwrap()
}

fn map(impl_reply: &str, review_reply: &str) -> HashMap<String, Box<dyn Adapter>> {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    m.insert(
        "codex".into(),
        Box::new(AlwaysOk {
            name: "codex".into(),
            reply: impl_reply.into(),
        }),
    );
    m.insert(
        "claude".into(),
        Box::new(AlwaysOk {
            name: "claude".into(),
            reply: review_reply.into(),
        }),
    );
    m
}

#[test]
fn stall_limit_breaks_on_no_progress_before_max_rounds() {
    // implementer returns byte-identical output every round; reviewer always CHANGES (never lands).
    let c = Conductor::new(
        crew_with_stall(2, 10),
        map("same output", "VERDICT: CHANGES: nope"),
    );
    let out = c.run("t", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(why) => assert!(why.contains("circuit-broken"), "got: {why}"),
        other => panic!("identical output must trip the breaker: {other:?}"),
    }
    assert!(
        out.rounds < 10,
        "must break EARLY, not grind to max_rounds (got {} rounds)",
        out.rounds
    );
}

#[test]
fn stall_limit_zero_is_disabled() {
    // stall_limit = 0 → no breaker; identical output just iterates to max_rounds then escalates
    // on the quorum (NOT "circuit-broken").
    let c = Conductor::new(
        crew_with_stall(0, 3),
        map("same output", "VERDICT: CHANGES: nope"),
    );
    let out = c.run("t", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(why) => assert!(!why.contains("circuit-broken"), "got: {why}"),
        other => panic!("expected a non-breaker escalation: {other:?}"),
    }
}

#[test]
fn abort_flag_stops_a_run_cleanly() {
    let flag = Arc::new(AtomicBool::new(true)); // pre-aborted
    let c = Conductor::new(crew_with_stall(0, 5), map("impl", "VERDICT: LGTM")).with_abort(flag);
    let out = c.run("t", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(why) => assert!(why.contains("aborted"), "got: {why}"),
        other => panic!("a set abort flag must stop the run: {other:?}"),
    }
    assert_eq!(
        out.rounds, 0,
        "abort at the first round boundary → 0 rounds"
    );
}
