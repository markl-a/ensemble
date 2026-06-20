//! Firewall A e2e: the conductor's test gate gates landing on a real (shell) test command.
//! Uses `grep` so these run on the WSL/Linux test runner (the project's build target).
#![cfg(unix)]

use ensemble::*;
use std::collections::HashMap;

/// Implementer writes `file` with `content` into cwd; an empty `file` means "reviewer" → LGTM.
struct Writer {
    name: String,
    file: String,
    content: String,
}
impl Adapter for Writer {
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

fn crew_with_test(cmd: &str) -> CrewConfig {
    CrewConfig::from_toml(&format!(
        r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 2
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
        [test]
        command = "{cmd}"
    "#
    ))
    .unwrap()
}

fn writers(content: &str) -> HashMap<String, Box<dyn Adapter>> {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    m.insert(
        "codex".into(),
        Box::new(Writer {
            name: "codex".into(),
            file: "ok.txt".into(),
            content: content.into(),
        }),
    );
    m.insert(
        "claude".into(),
        Box::new(Writer {
            name: "claude".into(),
            file: String::new(),
            content: String::new(),
        }),
    );
    m
}

#[test]
fn green_tests_allow_a_landing() {
    // implementer writes ok.txt="PASS"; the test command greps it → exit 0 (GREEN)
    let cwd = tempfile::tempdir().unwrap();
    let c = Conductor::new(crew_with_test("grep -q PASS ok.txt"), writers("PASS"));
    let out = c.run("do it", cwd.path());
    assert!(
        matches!(out.decision, Decision::Landed),
        "green tests + LGTM must land: {:?}",
        out.decision
    );
}

#[test]
fn red_tests_never_land_and_escalate() {
    // implementer writes ok.txt="NOPE"; the test command greps for PASS → exit 1 every round
    let cwd = tempfile::tempdir().unwrap();
    let c = Conductor::new(crew_with_test("grep -q PASS ok.txt"), writers("NOPE"));
    let out = c.run("do it", cwd.path());
    match out.decision {
        Decision::Escalated(why) => {
            assert!(why.contains("tests never passed"), "got: {why}")
        }
        other => panic!("red tests must escalate, never land: {other:?}"),
    }
}
