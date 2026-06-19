use ensemble::*;
use std::collections::HashMap;

/// A node-side adapter that writes a file in its cwd (the materialized base) then reports.
struct NodeWriter {
    name: String,
    file: String,
    content: String,
}
impl Adapter for NodeWriter {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        std::fs::write(cwd.join(&self.file), &self.content).unwrap();
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: format!("wrote {}", self.file),
        })
    }
}

struct AlwaysLgtm {
    name: String,
}
impl Adapter for AlwaysLgtm {
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

/// The whole Phase-3b-1 chain end to end: a REMOTE implementer (driven over an in-process
/// `ensemble serve`) edits a file on the node; those edits flow back into the orchestrator's
/// worktree and Phase-2c persists them on the kept branch.
#[test]
fn remote_implementer_edits_land_and_persist() {
    // a node hosting a file-writing "codex" over an in-process ensemble serve (one request)
    let mut node: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    node.insert(
        "codex".into(),
        Box::new(NodeWriter {
            name: "codex".into(),
            file: "feature.txt".into(),
            content: "REMOTE-FEATURE".into(),
        }),
    );
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let h = std::thread::spawn(move || ensemble::serve::serve_until_n(server, node, 1));

    // orchestrator repo; implementer = RemoteAdapter(url), reviewer = local always-LGTM
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
    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    map.insert("codex".into(), Box::new(RemoteAdapter::new("codex", &url)));
    map.insert(
        "claude".into(),
        Box::new(AlwaysLgtm {
            name: "claude".into(),
        }),
    );
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("add the feature", repo);
    assert!(
        matches!(out.decision, Decision::Landed),
        "should land: {:?}",
        out.decision
    );
    let branch = out.branch.expect("LANDED records a kept branch");
    let show = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{branch}:feature.txt")])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&show.stdout),
        "REMOTE-FEATURE",
        "the remote agent's edit must persist on the kept branch"
    );
    h.join().unwrap();
}
