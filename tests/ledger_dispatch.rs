use ensemble::ledger::Ledger;
use ensemble::*;
use std::collections::HashMap;

struct Writer {
    name: String,
    file: String,
}
impl Adapter for Writer {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        std::fs::write(cwd.join(&self.file), "X").unwrap();
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: format!("wrote {}", self.file),
        })
    }
}

struct Lgtm {
    name: String,
}
impl Adapter for Lgtm {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, _cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: "VERDICT: LGTM".into(),
        })
    }
}

fn crew() -> CrewConfig {
    CrewConfig::from_toml(
        r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#,
    )
    .unwrap()
}

fn git_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for a in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(a)
            .output()
            .unwrap();
    }
    std::fs::write(repo.join("seed"), "x").unwrap();
    for a in [&["add", "."][..], &["commit", "-q", "-m", "init"]] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(a)
            .output()
            .unwrap();
    }
    tmp
}

fn conductor() -> Conductor {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    m.insert(
        "codex".into(),
        Box::new(Writer {
            name: "codex".into(),
            file: "out.txt".into(),
        }),
    );
    m.insert(
        "claude".into(),
        Box::new(Lgtm {
            name: "claude".into(),
        }),
    );
    Conductor::new(crew(), m)
}

#[test]
fn dispatch_drains_all_tasks_to_done() {
    let tmp = git_repo();
    let mut ledger = Ledger::open(&tmp.path().join("ledger.db")).unwrap();
    let c = conductor();
    let tasks = vec!["task one".to_string(), "task two".to_string()];
    let counts =
        ensemble::dispatch::run(&mut ledger, &c, &tasks, tmp.path(), "w", &|| 1000, 0).unwrap();
    assert_eq!((counts.done, counts.failed, counts.queued), (2, 0, 0));
}

#[test]
fn dispatch_is_idempotent_and_recovers_orphans() {
    let tmp = git_repo();
    let path = tmp.path().join("ledger.db");
    let c = conductor();
    let tasks = vec!["only task".to_string()];

    // first run completes the task
    {
        let mut l = Ledger::open(&path).unwrap();
        let counts =
            ensemble::dispatch::run(&mut l, &c, &tasks, tmp.path(), "w", &|| 1000, 0).unwrap();
        assert_eq!(counts.done, 1);
    }
    // simulate a SECOND, crashed worker that claimed a NEW task but never finished it
    {
        let l = Ledger::open(&path).unwrap();
        l.enqueue("orphan", "left mid-flight", 2000).unwrap();
    }
    {
        let mut l = Ledger::open(&path).unwrap();
        l.claim("dead", 2000).unwrap(); // claims 'orphan', claimed_at = 2000, never completed
    }
    // re-run with now=5000, stale_before=4000 → orphan (claimed at 2000) recovered + completed;
    // the already-done task is NOT re-run (idempotent enqueue + it's terminal)
    {
        let mut l = Ledger::open(&path).unwrap();
        let counts =
            ensemble::dispatch::run(&mut l, &c, &tasks, tmp.path(), "w2", &|| 5000, 4000).unwrap();
        assert_eq!(
            counts.done, 2,
            "orphan recovered + completed; original stays done"
        );
        assert_eq!(counts.queued + counts.claimed, 0);
    }
}
