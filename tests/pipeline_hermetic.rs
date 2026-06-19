use ensemble::*;
use std::collections::HashMap;

fn crew() -> CrewConfig {
    CrewConfig::from_toml(
        r#"
        pipeline = ["implement", "review"]
        [gate]
        min_approvals = 1
        max_rounds = 2
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#,
    )
    .unwrap()
}

fn conductor(adapters: Vec<Box<dyn Adapter>>) -> Conductor {
    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    for a in adapters {
        map.insert(a.name().to_string(), a);
    }
    Conductor::new(crew(), map)
}

#[test]
fn happy_path_lands_on_first_round() {
    let c = conductor(vec![
        Box::new(MockAdapter::new("codex", vec![Ok("implemented".into())])),
        Box::new(MockAdapter::new("claude", vec![Ok("VERDICT: LGTM".into())])),
    ]);
    let out = c.run("add a function", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed));
    assert_eq!(out.rounds, 1);
    // the blackboard carried the implementer's result + the reviewer's verdict
    assert!(out.blackboard.len() >= 2);
}

#[test]
fn changes_then_lands_second_round() {
    let c = conductor(vec![
        Box::new(MockAdapter::new(
            "codex",
            vec![Ok("v1".into()), Ok("v2 fixed".into())],
        )),
        Box::new(MockAdapter::new(
            "claude",
            vec![
                Ok("VERDICT: CHANGES: handle empty".into()),
                Ok("VERDICT: LGTM".into()),
            ],
        )),
    ]);
    let out = c.run("task", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed));
    assert_eq!(out.rounds, 2);
}

#[test]
fn flaked_reviewer_is_excluded_and_escalates_not_fakes() {
    // the only reviewer flakes every round ⇒ quorum can never be met ⇒ Escalate (NOT Landed)
    let c = conductor(vec![
        Box::new(MockAdapter::new(
            "codex",
            vec![Ok("impl".into()), Ok("impl".into())],
        )),
        Box::new(MockAdapter::new(
            "claude",
            vec![Err(AdapterError::RateLimited), Err(AdapterError::Empty)],
        )),
    ]);
    let out = c.run("task", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(reason) => assert!(reason.to_lowercase().contains("reviewer")),
        other => panic!("a flaked reviewer must escalate, never land: {other:?}"),
    }
}
