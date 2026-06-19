use crate::crew::GatePolicy;
use crate::verdict::Verdict;

/// One reviewer's verdict (excluded/flaked reviewers are simply absent from the slice).
#[derive(Debug, Clone)]
pub struct RoleVerdict {
    pub role: String,
    pub agent: String,
    pub verdict: Verdict,
}

#[derive(Debug)]
pub enum GateDecision {
    Land,
    /// Not enough approvals but rounds remain — these change-messages go back to the implementer.
    Iterate(Vec<String>),
    Escalate(String),
}

/// Decide the fate of a round. `round` is 0-based. A flaked reviewer is NOT in `verdicts`, so an
/// all-flaked round has zero verdicts and ESCALATES — quorum is never faked from absent reviewers.
pub fn decide(verdicts: &[RoleVerdict], policy: &GatePolicy, round: u32) -> GateDecision {
    if verdicts.is_empty() {
        return GateDecision::Escalate(
            "no reviewers available (all excluded/flaked)".to_string(),
        );
    }
    let approvals = verdicts
        .iter()
        .filter(|v| matches!(v.verdict, Verdict::Approve))
        .count() as u32;
    if approvals >= policy.min_approvals {
        return GateDecision::Land;
    }
    if round + 1 >= policy.max_rounds {
        return GateDecision::Escalate(format!(
            "quorum not reached after {} round(s): {}/{} approvals",
            round + 1,
            approvals,
            policy.min_approvals
        ));
    }
    let changes: Vec<String> = verdicts
        .iter()
        .filter_map(|v| match &v.verdict {
            Verdict::Changes(m) => Some(format!("{} ({}): {}", v.role, v.agent, m)),
            Verdict::Approve => None,
        })
        .collect();
    GateDecision::Iterate(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::{GatePolicy, OnFlake};
    use crate::verdict::Verdict;

    fn policy(min: u32, rounds: u32) -> GatePolicy {
        GatePolicy {
            min_approvals: min,
            max_rounds: rounds,
            on_flake: OnFlake::Exclude,
        }
    }
    fn rv(role: &str, agent: &str, v: Verdict) -> RoleVerdict {
        RoleVerdict {
            role: role.into(),
            agent: agent.into(),
            verdict: v,
        }
    }

    #[test]
    fn lands_on_quorum() {
        let vs = vec![
            rv("review", "claude", Verdict::Approve),
            rv("debug", "agy", Verdict::Approve),
        ];
        assert!(matches!(decide(&vs, &policy(2, 2), 0), GateDecision::Land));
    }
    #[test]
    fn iterates_with_changes_when_rounds_remain() {
        let vs = vec![
            rv("review", "claude", Verdict::Changes("fix x".into())),
            rv("debug", "agy", Verdict::Approve),
        ];
        match decide(&vs, &policy(2, 2), 0) {
            GateDecision::Iterate(msgs) => assert!(msgs.iter().any(|m| m.contains("fix x"))),
            other => panic!("expected Iterate, got {other:?}"),
        }
    }
    #[test]
    fn escalates_when_rounds_exhausted() {
        let vs = vec![rv("review", "claude", Verdict::Changes("nope".into()))];
        assert!(matches!(
            decide(&vs, &policy(2, 1), 0),
            GateDecision::Escalate(_)
        ));
    }
    #[test]
    fn escalates_when_no_reviewers_left() {
        // all reviewers were excluded (flaked) ⇒ empty ⇒ never fake a land
        assert!(matches!(
            decide(&[], &policy(1, 3), 0),
            GateDecision::Escalate(_)
        ));
    }
}
