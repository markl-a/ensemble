use ensemble::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

struct CwdProbe {
    name: String,
    reply: String,
    seen: Arc<Mutex<Vec<std::path::PathBuf>>>,
}
impl Adapter for CwdProbe {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        self.seen.lock().unwrap().push(cwd.to_path_buf());
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: self.reply.clone(),
        })
    }
}

#[test]
fn run_in_repo_runs_inside_a_worktree_then_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
    }
    std::fs::write(repo.join("f"), "x").unwrap();
    for args in [&["add", "."][..], &["commit", "-q", "-m", "init"]] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(CwdProbe {
            name: "codex".into(),
            reply: "impl".into(),
            seen: seen.clone(),
        }),
    );
    map.insert(
        "claude".into(),
        Box::new(CwdProbe {
            name: "claude".into(),
            reply: "VERDICT: LGTM".into(),
            seen: seen.clone(),
        }),
    );
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("add a fn", repo);
    assert!(matches!(out.decision, Decision::Landed));
    // every adapter ran inside the worktree, not the repo root
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .all(|p| p.to_string_lossy().contains("worktrees")),
        "ran outside worktree: {seen:?}"
    );
    // worktree cleaned up
    assert!(!repo.join(".ensemble/worktrees").join("add-a-fn").exists());
}

// reusable always-ok adapter
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

#[test]
fn run_many_runs_tasks_in_parallel_each_in_its_own_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
    }
    std::fs::write(repo.join("f"), "x").unwrap();
    for args in [&["add", "."][..], &["commit", "-q", "-m", "init"]] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
    }
    // each adapter call returns Ok; reviewer always LGTM ⇒ all land
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(AlwaysOk {
            name: "codex".into(),
            reply: "impl".into(),
        }),
    );
    map.insert(
        "claude".into(),
        Box::new(AlwaysOk {
            name: "claude".into(),
            reply: "VERDICT: LGTM".into(),
        }),
    );
    let c = Conductor::new(crew(), map);

    let outs = c.run_many(
        &["task one".into(), "task two".into(), "task three".into()],
        repo,
    );
    assert_eq!(outs.len(), 3);
    assert!(outs.iter().all(|o| matches!(o.decision, Decision::Landed)));
    // all worktrees cleaned up
    let live = std::fs::read_dir(repo.join(".ensemble/worktrees"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(live, 0, "worktrees should be cleaned up");
}
