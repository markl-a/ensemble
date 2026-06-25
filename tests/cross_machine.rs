use ensemble::*;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// A node-side adapter that writes a file in its cwd (the materialized base) then reports.
struct NodeWriter {
    name: String,
    file: String,
    content: String,
    hits: Option<Arc<AtomicUsize>>,
}
impl Adapter for NodeWriter {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        if let Some(hits) = &self.hits {
            hits.fetch_add(1, Ordering::SeqCst);
        }
        std::fs::write(cwd.join(&self.file), &self.content).unwrap();
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: format!("wrote {}", self.file),
        })
    }
}

struct AlwaysLgtm {
    name: String,
    hits: Option<Arc<AtomicUsize>>,
}
impl Adapter for AlwaysLgtm {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _p: &str, _cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        if let Some(hits) = &self.hits {
            hits.fetch_add(1, Ordering::SeqCst);
        }
        Ok(AgentOutput {
            agent: self.name.clone(),
            text: "VERDICT: LGTM".into(),
        })
    }
}

fn node_writer(name: &str, file: &str, content: &str) -> Box<dyn Adapter> {
    Box::new(NodeWriter {
        name: name.into(),
        file: file.into(),
        content: content.into(),
        hits: None,
    })
}

fn counted_node_writer(
    name: &str,
    file: &str,
    content: &str,
    hits: Arc<AtomicUsize>,
) -> Box<dyn Adapter> {
    Box::new(NodeWriter {
        name: name.into(),
        file: file.into(),
        content: content.into(),
        hits: Some(hits),
    })
}

fn always_lgtm(name: &str) -> Box<dyn Adapter> {
    Box::new(AlwaysLgtm {
        name: name.into(),
        hits: None,
    })
}

fn counted_lgtm(name: &str, hits: Arc<AtomicUsize>) -> Box<dyn Adapter> {
    Box::new(AlwaysLgtm {
        name: name.into(),
        hits: Some(hits),
    })
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

fn phase2_crew_with_test() -> CrewConfig {
    CrewConfig::from_toml(&format!(
        r#"
        pipeline = ["implement","review","audit"]
        [gate]
        min_approvals = 2
        max_rounds = 1
        on_flake = "exclude"
        [test]
        command = "{}"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
        [roles.audit]
        agent = "agy"
    "#,
        test_command_for_ok_file()
    ))
    .unwrap()
}

fn phase2_single_reviewer_crew_with_test() -> CrewConfig {
    CrewConfig::from_toml(&format!(
        r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "exclude"
        [test]
        command = "{}"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#,
        test_command_for_ok_file()
    ))
    .unwrap()
}

fn phase2_duplicate_vendor_crew_with_test() -> CrewConfig {
    CrewConfig::from_toml(&format!(
        r#"
        pipeline = ["implement","review","audit"]
        [gate]
        min_approvals = 2
        max_rounds = 1
        on_flake = "exclude"
        [test]
        command = "{}"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
        [roles.audit]
        agent = "claude"
    "#,
        test_command_for_ok_file()
    ))
    .unwrap()
}

#[cfg(windows)]
fn test_command_for_ok_file() -> &'static str {
    "if exist ok.txt (exit /b 0) else (exit /b 1)"
}

#[cfg(not(windows))]
fn test_command_for_ok_file() -> &'static str {
    "test -f ok.txt"
}

fn init_repo(repo: &std::path::Path) {
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
        node_writer("codex", "feature.txt", "REMOTE-FEATURE"),
    );
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let h = std::thread::spawn(move || ensemble::serve::serve_until_n(server, node, 1));

    // orchestrator repo; implementer = RemoteAdapter(url), reviewer = local always-LGTM
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    map.insert("codex".into(), Box::new(RemoteAdapter::new("codex", &url)));
    map.insert("claude".into(), always_lgtm("claude"));
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

#[test]
fn remote_run_does_not_land_when_the_test_gate_fails() {
    let codex_hits = Arc::new(AtomicUsize::new(0));
    let mut node: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    node.insert(
        "codex".into(),
        counted_node_writer("codex", "not-ok.txt", "FAIL", codex_hits.clone()),
    );
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let h = std::thread::spawn(move || ensemble::serve::serve_until_n(server, node, 1));

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);

    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    map.insert("codex".into(), Box::new(RemoteAdapter::new("codex", &url)));
    map.insert("claude".into(), always_lgtm("claude"));
    let c = Conductor::new(phase2_single_reviewer_crew_with_test(), map);

    let out = c.run_in_repo("try to add ok.txt through the remote node", repo);
    match out.decision {
        Decision::Escalated(ref why) => {
            assert!(why.contains("tests never passed"), "got: {why}");
        }
        Decision::Landed => panic!("a failed test gate must never land"),
    }
    assert!(
        out.branch.is_none(),
        "ESCALATED failed-test runs must not keep a landing branch"
    );
    let msgs = out.blackboard.read_since(0);
    assert!(
        msgs.iter()
            .any(|m| m.from == "test" && m.kind == "test_failure"),
        "test failure must be recorded: {msgs:?}"
    );
    assert!(
        !msgs.iter().any(|m| m.kind == "verdict"),
        "reviewers must not run after a red test gate: {msgs:?}"
    );
    assert_eq!(
        codex_hits.load(Ordering::SeqCst),
        1,
        "remote implementer must run before the red test gate"
    );
    h.join().unwrap();
}

#[test]
fn duplicate_reviewer_vendor_does_not_satisfy_phase2_quorum() {
    let codex_hits = Arc::new(AtomicUsize::new(0));
    let claude_hits = Arc::new(AtomicUsize::new(0));
    let mut node: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    node.insert(
        "codex".into(),
        counted_node_writer("codex", "ok.txt", "PASS", codex_hits.clone()),
    );
    node.insert("claude".into(), counted_lgtm("claude", claude_hits.clone()));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let h = std::thread::spawn(move || ensemble::serve::serve_until_n(server, node, 3));

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);

    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    map.insert("codex".into(), Box::new(RemoteAdapter::new("codex", &url)));
    map.insert(
        "claude".into(),
        Box::new(RemoteAdapter::new("claude", &url)),
    );
    let c = Conductor::new(phase2_duplicate_vendor_crew_with_test(), map);

    let out = c.run_in_repo("add ok.txt but use one reviewer vendor twice", repo);
    match out.decision {
        Decision::Escalated(ref why) => {
            assert!(why.contains("quorum not reached"), "got: {why}");
        }
        Decision::Landed => panic!("two roles from one vendor must not satisfy min_approvals=2"),
    }
    let msgs = out.blackboard.read_since(0);
    let verdicts = msgs
        .iter()
        .filter(|m| m.from == "claude" && m.kind == "verdict")
        .count();
    assert_eq!(
        verdicts, 2,
        "both reviewer roles ran, but only one distinct vendor approved: {msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.from == "conductor" && m.kind == "decision" && m.body.contains("escalated")),
        "terminal escalation decision missing: {msgs:?}"
    );
    assert_eq!(
        codex_hits.load(Ordering::SeqCst),
        1,
        "remote implementer should run exactly once"
    );
    assert_eq!(
        claude_hits.load(Ordering::SeqCst),
        2,
        "duplicate-vendor reviewers should both run through RemoteAdapter"
    );
    h.join().unwrap();
}

/// Phase 2 Slice B governance in one process: the implementer and both reviewers are REMOTE
/// adapters driven through `serve`, the test gate must pass before reviewers run, and the task
/// only lands after two distinct reviewer vendors approve.
#[test]
fn remote_run_requires_test_pass_and_two_remote_reviewer_approvals() {
    let mut node: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    node.insert("codex".into(), node_writer("codex", "ok.txt", "PASS"));
    node.insert("claude".into(), always_lgtm("claude"));
    node.insert("agy".into(), always_lgtm("agy"));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let h = std::thread::spawn(move || ensemble::serve::serve_until_n(server, node, 3));

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);

    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    map.insert("codex".into(), Box::new(RemoteAdapter::new("codex", &url)));
    map.insert(
        "claude".into(),
        Box::new(RemoteAdapter::new("claude", &url)),
    );
    map.insert("agy".into(), Box::new(RemoteAdapter::new("agy", &url)));
    let c = Conductor::new(phase2_crew_with_test(), map);

    let out = c.run_in_repo("add ok.txt through the remote node", repo);
    assert!(
        matches!(out.decision, Decision::Landed),
        "remote implementer + test gate + two remote reviewers must land: {:?}",
        out.decision
    );
    let msgs = out.blackboard.read_since(0);
    assert!(
        msgs.iter()
            .any(|m| m.from == "test" && m.kind == "test_pass"),
        "test gate must pass before reviewer quorum: {msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.from == "claude" && m.kind == "verdict"),
        "claude remote reviewer verdict missing: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.from == "agy" && m.kind == "verdict"),
        "agy remote reviewer verdict missing: {msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.from == "conductor" && m.kind == "decision" && m.body == "LANDED"),
        "terminal LANDED decision missing: {msgs:?}"
    );
    let position = |from: &str, kind: &str| {
        msgs.iter()
            .position(|m| m.from == from && m.kind == kind)
            .unwrap_or_else(|| panic!("missing {from}/{kind}: {msgs:?}"))
    };
    let test_i = position("test", "test_pass");
    let claude_i = position("claude", "verdict");
    let agy_i = position("agy", "verdict");
    let decision_i = position("conductor", "decision");
    assert!(
        test_i < claude_i && test_i < agy_i && claude_i < decision_i && agy_i < decision_i,
        "test gate must run before reviewers, and reviewers before LANDED: {msgs:?}"
    );

    let branch = out.branch.expect("LANDED records a kept branch");
    let show = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{branch}:ok.txt")])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&show.stdout), "PASS");
    h.join().unwrap();
}
