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
            vec![
                Err(AdapterError::RateLimited(RateLimitInfo::default())),
                Err(AdapterError::Empty),
            ],
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
    // worktree cleaned up (the worktrees dir is empty after the run)
    let live = std::fs::read_dir(repo.join(".ensemble/worktrees"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(live, 0, "worktree should be cleaned up");
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

#[test]
fn on_flake_retry_recovers_after_one_transient_flake() {
    // reviewer flakes once then approves; on_flake=retry ⇒ the round still gets an APPROVE ⇒ Land
    let crew = CrewConfig::from_toml(
        r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "retry"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#,
    )
    .unwrap();
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(MockAdapter::new("codex", vec![Ok("impl".into())])),
    );
    map.insert(
        "claude".into(),
        Box::new(MockAdapter::new(
            "claude",
            vec![
                Err(AdapterError::RateLimited(RateLimitInfo::default())),
                Ok("VERDICT: LGTM".into()),
            ],
        )),
    );
    let out = Conductor::new(crew, map).run("t", std::path::Path::new("."));
    assert!(
        matches!(out.decision, Decision::Landed),
        "retry must recover the transient flake"
    );
}

#[test]
fn on_flake_substitute_uses_the_backup_agent() {
    let crew = CrewConfig::from_toml(
        r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "substitute"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
        [agents.claude]
        backup = "opencode"
    "#,
    )
    .unwrap();
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(MockAdapter::new("codex", vec![Ok("impl".into())])),
    );
    map.insert(
        "claude".into(),
        Box::new(MockAdapter::new("claude", vec![Err(AdapterError::Empty)])),
    );
    map.insert(
        "opencode".into(),
        Box::new(MockAdapter::new(
            "opencode",
            vec![Ok("VERDICT: LGTM".into())],
        )),
    );
    let out = Conductor::new(crew, map).run("t", std::path::Path::new("."));
    assert!(
        matches!(out.decision, Decision::Landed),
        "substitute must fall back to the backup agent"
    );
}

// Writes `file` (with `content`) into cwd when non-empty, else just LGTMs — lets us model an
// implementer that produces a real file then a reviewer that approves it.
struct WriterThenLgtm {
    name: String,
    file: String,
    content: String,
}
impl Adapter for WriterThenLgtm {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        if !self.file.is_empty() {
            std::fs::write(cwd.join(&self.file), &self.content).unwrap();
        }
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: if self.file.is_empty() {
                "VERDICT: LGTM".into()
            } else {
                format!("wrote {}", self.file)
            },
        })
    }
}

#[test]
fn landed_run_persists_work_on_a_kept_branch() {
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
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(WriterThenLgtm {
            name: "codex".into(),
            file: "out.txt".into(),
            content: "DONE".into(),
        }),
    );
    map.insert(
        "claude".into(),
        Box::new(WriterThenLgtm {
            name: "claude".into(),
            file: String::new(),
            content: String::new(),
        }),
    );
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("write out.txt", repo);
    assert!(matches!(out.decision, Decision::Landed));
    let branch = out
        .branch
        .clone()
        .expect("LANDED must record a kept branch");
    // the branch exists and carries out.txt = DONE
    let show = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{branch}:out.txt")])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&show.stdout), "DONE");
}

#[test]
fn run_in_repo_writes_a_journal_of_the_collaboration() {
    // design step 2: a worktree run records the blackboard transcript + a terminal decision to
    // `.ensemble/runs/<slug>.jsonl`, so the operator can replay what the crew did.
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
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(WriterThenLgtm {
            name: "codex".into(),
            file: "out.txt".into(),
            content: "DONE".into(),
        }),
    );
    map.insert(
        "claude".into(),
        Box::new(WriterThenLgtm {
            name: "claude".into(),
            file: String::new(),
            content: String::new(),
        }),
    );
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("write out.txt", repo);
    assert!(matches!(out.decision, Decision::Landed));

    // exactly one run was journaled
    let files: Vec<_> = std::fs::read_dir(repo.join(".ensemble/runs"))
        .expect("a .ensemble/runs dir should be created")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    assert_eq!(files.len(), 1, "exactly one run journal: {files:?}");

    let body = std::fs::read_to_string(&files[0]).unwrap();
    let entries = ensemble::parse_journal(&body).expect("journal parses");
    // the implementer's result and the reviewer's verdict are both recorded as Msg entries
    assert!(
        entries.iter().any(|e| matches!(
            e, ensemble::JournalEntry::Msg(m) if m.from == "codex" && m.kind == "result"
        )),
        "implementer result must be journaled"
    );
    assert!(
        entries.iter().any(|e| matches!(
            e, ensemble::JournalEntry::Msg(m) if m.body.contains("VERDICT: LGTM")
        )),
        "reviewer verdict must be journaled"
    );
    // the LAST entry is the terminal landed decision, carrying the kept branch
    match entries.last().expect("journal is non-empty") {
        ensemble::JournalEntry::Decision {
            outcome, detail, ..
        } => {
            assert_eq!(outcome, "landed");
            assert_eq!(detail.as_str(), out.branch.as_deref().unwrap());
        }
        other => panic!("last journal entry must be the decision: {other:?}"),
    }
}

#[test]
fn commit_failure_escalates_never_a_silent_land() {
    // The persistence feature's whole point is "LANDED work survives". If the commit FAILS the
    // branch is discarded on Drop, so the work is lost — the run must Escalate (not report a clean
    // Landed with exit 0), or the headline/exit-code would lie about destroyed work.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for a in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
        // Force every commit to fail: require GPG signing but point at a gpg that doesn't exist.
        // Deterministic and cross-platform — `git add`/`git diff` are unaffected, `git commit` is.
        &["config", "commit.gpgsign", "true"],
        &["config", "gpg.program", "definitely-not-a-real-gpg-binary"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(a)
            .output()
            .unwrap();
    }
    // seed commit (made before gpgsign matters? no — it's already on; sign-disable just this one)
    std::fs::write(repo.join("seed"), "x").unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", "."])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"])
        .output()
        .unwrap();

    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> =
        std::collections::HashMap::new();
    map.insert(
        "codex".into(),
        Box::new(WriterThenLgtm {
            name: "codex".into(),
            file: "out.txt".into(),
            content: "DONE".into(),
        }),
    );
    map.insert(
        "claude".into(),
        Box::new(WriterThenLgtm {
            name: "claude".into(),
            file: String::new(),
            content: String::new(),
        }),
    );
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("write out.txt", repo);
    // the gate LANDED, but persisting failed ⇒ the outcome must be Escalated, not a silent Land
    assert!(
        matches!(out.decision, Decision::Escalated(_)),
        "commit failure must escalate, not report a clean Landed: {:?}",
        out.decision
    );
    assert!(
        out.branch.is_none(),
        "no branch should be recorded when the work could not be persisted"
    );
}
