# ensemble Phase-1 Implementation Plan — single-machine role pipeline + blackboard

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development (or executing-plans). Steps use `- [ ]` checkboxes. Build/test with **`cargo test`** (native; this is a tiny crate). Work in `D:\Projects\ensemble`.

**Goal:** A hermetic, fully-tested single-machine role pipeline — a task flows implement → review → debug across (mock) vendor agents that communicate through a blackboard, gated by a quorum that degrades (never fakes) when an agent flakes — plus one real adapter (codex) behind an `#[ignore]` live smoke.

**Architecture:** Small focused modules behind an `Adapter` trait so the entire pipeline is testable with a `MockAdapter` (no live CLI). `Conductor` runs the pipeline, routing inter-agent messages via an append-only `Blackboard` (rolling summary injected into each agent's prompt); `Gate` turns reviewer verdicts into Land/Iterate/Escalate; a flaked reviewer is **excluded** from quorum with a logged reason, never counted as approval.

**Tech Stack:** Rust 2021, `serde` + `toml` (config), `thiserror` (typed errors). CLI arg parsing hand-rolled (one subcommand). No async in Phase 1.

**Spec:** `docs/2026-06-19-ensemble-design.md` (§3 architecture, §4a comms/adapter findings).

---

### Task 1: Crate skeleton, deps, lib root

**Files:**
- Modify: `Cargo.toml`
- Create: `src/lib.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add deps to `Cargo.toml`** (under `[dependencies]`):

```toml
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "1"
```

- [ ] **Step 2: Create `src/lib.rs`** declaring the modules and re-exports:

```rust
//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.

pub mod adapter;
pub mod blackboard;
pub mod conductor;
pub mod crew;
pub mod gate;
pub mod verdict;

pub use adapter::{Adapter, AdapterError, AgentOutput, MockAdapter};
pub use blackboard::{Blackboard, Message};
pub use conductor::{Conductor, Decision, RunOutcome};
pub use crew::{CrewConfig, GatePolicy, OnFlake, RoleConfig};
pub use gate::{decide, GateDecision, RoleVerdict};
pub use verdict::{parse_verdict, Verdict};
```

- [ ] **Step 3: Verify it builds** — Run: `cargo build`. Expected: fails (modules don't exist yet). That's fine; the next tasks create them. To keep `main.rs` compiling meanwhile, leave the existing stub `main()`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/lib.rs
git commit -m "chore(phase1): add deps + lib module skeleton"
```

---

### Task 2: `Adapter` trait, typed errors, `MockAdapter`

**Files:**
- Create: `src/adapter.rs`

- [ ] **Step 1: Write the failing test** (append a `#[cfg(test)] mod tests` to `src/adapter.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn mock_returns_scripted_then_flakes() {
        let m = MockAdapter::new("codex", vec![
            Ok("I implemented the change.".to_string()),
            Err(AdapterError::Empty),
        ]);
        assert_eq!(m.name(), "codex");
        let out = m.run("do the thing", Path::new(".")).unwrap();
        assert_eq!(out.agent, "codex");
        assert_eq!(out.text, "I implemented the change.");
        assert!(matches!(m.run("again", Path::new(".")), Err(AdapterError::Empty)));
    }

    #[test]
    fn mock_exhausted_returns_empty() {
        let m = MockAdapter::new("claude", vec![]);
        assert!(matches!(m.run("x", Path::new(".")), Err(AdapterError::Empty)));
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — Run: `cargo test --lib adapter`. Expected: compile error (types undefined).

- [ ] **Step 3: Implement `src/adapter.rs`** (above the test module):

```rust
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

/// What an agent produced on one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutput {
    pub agent: String,
    pub text: String,
}

/// Why an agent did NOT produce a usable answer. These are the degrade signals: the gate
/// must treat any of them as "this reviewer is unavailable", never as approval.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("agent flaked: {0}")]
    Flaked(String),
    #[error("agent produced empty output")]
    Empty,
    #[error("agent rate-limited / quota exhausted")]
    RateLimited,
    #[error("agent CLI not installed: {0}")]
    NotInstalled(String),
}

/// A vendor AI CLI driven headlessly. Implementors encode the per-vendor invocation contract.
pub trait Adapter: Send + Sync {
    /// The agent's name as referenced in crew.toml (e.g. "codex", "claude").
    fn name(&self) -> &str;
    /// Run one turn: hand `prompt` to the agent with working dir `cwd`, return its reply.
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError>;
}

/// A scripted adapter for hermetic tests: returns successive queued responses; an exhausted
/// queue yields `AdapterError::Empty` so tests can model an agent that stops responding.
pub struct MockAdapter {
    name: String,
    responses: Mutex<VecDeque<Result<String, AdapterError>>>,
}

impl MockAdapter {
    pub fn new(name: &str, responses: Vec<Result<String, AdapterError>>) -> Self {
        Self { name: name.to_string(), responses: Mutex::new(responses.into()) }
    }
}

impl Adapter for MockAdapter {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _prompt: &str, _cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let mut q = self.responses.lock().unwrap();
        match q.pop_front() {
            Some(Ok(text)) => Ok(AgentOutput { agent: self.name.clone(), text }),
            Some(Err(e)) => Err(e),
            None => Err(AdapterError::Empty),
        }
    }
}
```

- [ ] **Step 4: Run, expect PASS** — Run: `cargo test --lib adapter`. Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/adapter.rs
git commit -m "feat(phase1): Adapter trait + typed AdapterError + MockAdapter"
```

---

### Task 3: `Verdict` + `parse_verdict`

**Files:**
- Create: `src/verdict.rs`

- [ ] **Step 1: Write the failing test:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_approve_and_changes_conservatively() {
        assert_eq!(parse_verdict("looks good\nVERDICT: LGTM"), Verdict::Approve);
        assert_eq!(parse_verdict("VERDICT: APPROVE"), Verdict::Approve);
        assert_eq!(
            parse_verdict("issues...\nVERDICT: CHANGES: fix the off-by-one"),
            Verdict::Changes("fix the off-by-one".to_string())
        );
        // No marker at all ⇒ conservative: NOT an approval (an unparseable review can't land).
        assert_eq!(
            parse_verdict("I think it is fine"),
            Verdict::Changes("no explicit VERDICT line; treating as changes-requested".to_string())
        );
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test --lib verdict`. Expected: compile error.

- [ ] **Step 3: Implement `src/verdict.rs`:**

```rust
/// A reviewer's decision on a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    /// Changes requested; the String is the message routed back to the implementer.
    Changes(String),
}

/// Parse an agent's reply into a verdict. Convention: a line `VERDICT: LGTM|APPROVE` approves;
/// `VERDICT: CHANGES: <msg>` requests changes. Anything without an explicit approving VERDICT
/// line is treated as changes-requested — an unparseable or ambiguous review must NEVER land.
pub fn parse_verdict(text: &str) -> Verdict {
    for line in text.lines() {
        let l = line.trim();
        let upper = l.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("VERDICT:") {
            let rest = rest.trim();
            if rest.starts_with("LGTM") || rest.starts_with("APPROVE") {
                return Verdict::Approve;
            }
            if let Some(idx) = rest.find("CHANGES") {
                // take the message after "CHANGES:" from the ORIGINAL-case line
                let after = l[l.to_ascii_uppercase().find("CHANGES").unwrap() + "CHANGES".len()..]
                    .trim_start_matches(|c: char| c == ':' || c.is_whitespace());
                let _ = idx;
                return Verdict::Changes(after.to_string());
            }
            return Verdict::Changes(format!("unrecognized VERDICT line: {l}"));
        }
    }
    Verdict::Changes("no explicit VERDICT line; treating as changes-requested".to_string())
}
```

- [ ] **Step 4: Run, expect PASS** — `cargo test --lib verdict`. Expected: 1 passed.

- [ ] **Step 5: Commit** — `git add src/verdict.rs && git commit -m "feat(phase1): Verdict + conservative parse_verdict"`

---

### Task 4: `Blackboard` (inter-agent comms)

**Files:**
- Create: `src/blackboard.rs`

- [ ] **Step 1: Write the failing test:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts_reads_and_summarizes() {
        let mut bb = Blackboard::new();
        assert!(bb.summary().is_empty());
        bb.post("codex", "result", "implemented the parser");
        bb.post("claude", "verdict", "VERDICT: CHANGES: handle empty input");
        assert_eq!(bb.len(), 2);
        let s = bb.summary();
        assert!(s.contains("codex"));
        assert!(s.contains("implemented the parser"));
        assert!(s.contains("claude"));
        // read_since returns only newer messages
        assert_eq!(bb.read_since(1).len(), 1);
        assert_eq!(bb.read_since(1)[0].from, "claude");
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test --lib blackboard`.

- [ ] **Step 3: Implement `src/blackboard.rs`:**

```rust
/// One message agents leave for each other on a task-run's shared channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub from: String,
    pub kind: String, // "result" | "verdict" | "finding" | "question"
    pub body: String,
}

/// Append-only per-task-run shared channel. Agents can't talk directly (they are subprocesses),
/// so each posts here and the conductor injects `summary()` into the next agent's prompt — the
/// mediated-blackboard inter-agent-comms pattern (design §4a, borrowed from bernstein).
#[derive(Debug, Default)]
pub struct Blackboard {
    msgs: Vec<Message>,
}

impl Blackboard {
    pub fn new() -> Self { Self::default() }
    pub fn post(&mut self, from: &str, kind: &str, body: &str) {
        self.msgs.push(Message { from: from.to_string(), kind: kind.to_string(), body: body.to_string() });
    }
    pub fn len(&self) -> usize { self.msgs.len() }
    pub fn is_empty(&self) -> bool { self.msgs.is_empty() }
    /// Messages at index >= `n`.
    pub fn read_since(&self, n: usize) -> &[Message] {
        let n = n.min(self.msgs.len());
        &self.msgs[n..]
    }
    /// A compact rolling summary injected into the next agent's prompt. Bodies are excerpted to
    /// keep the prompt budget bounded.
    pub fn summary(&self) -> String {
        if self.msgs.is_empty() { return String::new(); }
        let mut s = String::from("Other agents are working on this task. Recent activity:\n");
        for m in &self.msgs {
            let body = excerpt(&m.body, 400);
            s.push_str(&format!("- {} [{}]: {}\n", m.from, m.kind, body));
        }
        s
    }
}

fn excerpt(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max { s } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
```

- [ ] **Step 4: Run, expect PASS** — `cargo test --lib blackboard`.

- [ ] **Step 5: Commit** — `git add src/blackboard.rs && git commit -m "feat(phase1): Blackboard — mediated inter-agent comms channel"`

---

### Task 5: `crew.toml` parsing

**Files:**
- Create: `src/crew.rs`

- [ ] **Step 1: Write the failing test** (uses the repo's `examples/crew.toml`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipeline_roles_and_gate() {
        let toml = r#"
            [gate]
            min_approvals = 2
            max_rounds = 2
            on_flake = "exclude"
            pipeline = ["implement", "review", "debug"]
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
            blind = true
            [roles.debug]
            agent = "agy"
        "#;
        let c = CrewConfig::from_toml(toml).unwrap();
        assert_eq!(c.pipeline, vec!["implement", "review", "debug"]);
        assert_eq!(c.gate.min_approvals, 2);
        assert_eq!(c.gate.max_rounds, 2);
        assert!(matches!(c.gate.on_flake, OnFlake::Exclude));
        assert_eq!(c.roles["implement"].agent, "codex");
        assert!(c.roles["review"].blind);
        assert!(!c.roles["debug"].blind);
        // implementer = first pipeline role; reviewers = the rest
        assert_eq!(c.implementer_role(), "implement");
        assert_eq!(c.reviewer_roles(), vec!["review", "debug"]);
    }

    #[test]
    fn rejects_unknown_on_flake_in_phase1() {
        let toml = r#"
            [gate]
            min_approvals = 1
            max_rounds = 1
            on_flake = "substitute"
            pipeline = ["implement", "review"]
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
        "#;
        assert!(CrewConfig::from_toml(toml).is_err());
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test --lib crew`.

- [ ] **Step 3: Implement `src/crew.rs`:**

```rust
use serde::Deserialize;
use std::collections::HashMap;

/// What to do when a reviewer agent flakes. Phase 1 implements only `Exclude` (drop it from the
/// quorum with a logged reason — never fake a pass). `Retry`/`Substitute` are Phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFlake { Exclude }

#[derive(Debug, Clone, Deserialize)]
pub struct GatePolicy {
    pub min_approvals: u32,
    pub max_rounds: u32,
    #[serde(deserialize_with = "de_on_flake")]
    pub on_flake: OnFlake,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoleConfig {
    pub agent: String,
    #[serde(default)]
    pub blind: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CrewConfig {
    pub gate: GatePolicy,
    pub pipeline: Vec<String>,
    pub roles: HashMap<String, RoleConfig>,
}

fn de_on_flake<'de, D: serde::Deserializer<'de>>(d: D) -> Result<OnFlake, D::Error> {
    let s = String::deserialize(d)?;
    match s.as_str() {
        "exclude" => Ok(OnFlake::Exclude),
        other => Err(serde::de::Error::custom(format!(
            "on_flake = \"{other}\" is not supported in Phase 1 (only \"exclude\")"
        ))),
    }
}

impl CrewConfig {
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        let c: CrewConfig = toml::from_str(s)?;
        Ok(c)
    }
    pub fn from_path(p: &std::path::Path) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(p)?;
        Self::from_toml(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
    /// First pipeline role = the implementer.
    pub fn implementer_role(&self) -> &str { &self.pipeline[0] }
    /// All pipeline roles after the implementer = reviewers (their verdicts feed the gate).
    pub fn reviewer_roles(&self) -> Vec<&str> { self.pipeline.iter().skip(1).map(|s| s.as_str()).collect() }
}

use serde::de::Deserialize as _;
```

> Note: `examples/crew.toml` currently nests `pipeline` under no table at top level; ensure it parses (top-level `pipeline = [...]` is fine with this struct). If `serde` rejects the `use serde::de::Deserialize as _;` line, remove it — `String::deserialize` resolves via the `Deserialize` trait already in scope through `serde`.

- [ ] **Step 4: Run, expect PASS** — `cargo test --lib crew`. Fix the `examples/crew.toml` if needed so a `CrewConfig::from_path` smoke also parses (add a test `parses_repo_example` reading `examples/crew.toml`).

- [ ] **Step 5: Commit** — `git add src/crew.rs && git commit -m "feat(phase1): crew.toml parsing (Phase-1 on_flake=exclude only)"`

---

### Task 6: `Gate` quorum decision

**Files:**
- Create: `src/gate.rs`

- [ ] **Step 1: Write the failing test:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::{GatePolicy, OnFlake};
    use crate::verdict::Verdict;

    fn policy(min: u32, rounds: u32) -> GatePolicy {
        GatePolicy { min_approvals: min, max_rounds: rounds, on_flake: OnFlake::Exclude }
    }
    fn rv(role: &str, agent: &str, v: Verdict) -> RoleVerdict {
        RoleVerdict { role: role.into(), agent: agent.into(), verdict: v }
    }

    #[test]
    fn lands_on_quorum() {
        let vs = vec![rv("review","claude",Verdict::Approve), rv("debug","agy",Verdict::Approve)];
        assert!(matches!(decide(&vs, &policy(2,2), 0), GateDecision::Land));
    }
    #[test]
    fn iterates_with_changes_when_rounds_remain() {
        let vs = vec![rv("review","claude",Verdict::Changes("fix x".into())), rv("debug","agy",Verdict::Approve)];
        match decide(&vs, &policy(2,2), 0) {
            GateDecision::Iterate(msgs) => assert!(msgs.iter().any(|m| m.contains("fix x"))),
            other => panic!("expected Iterate, got {other:?}"),
        }
    }
    #[test]
    fn escalates_when_rounds_exhausted() {
        let vs = vec![rv("review","claude",Verdict::Changes("nope".into()))];
        assert!(matches!(decide(&vs, &policy(2,1), 0), GateDecision::Escalate(_)));
    }
    #[test]
    fn escalates_when_no_reviewers_left() {
        // all reviewers were excluded (flaked) ⇒ empty ⇒ never fake a land
        assert!(matches!(decide(&[], &policy(1,3), 0), GateDecision::Escalate(_)));
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test --lib gate`.

- [ ] **Step 3: Implement `src/gate.rs`:**

```rust
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
        return GateDecision::Escalate("no reviewers available (all excluded/flaked)".to_string());
    }
    let approvals = verdicts.iter().filter(|v| matches!(v.verdict, Verdict::Approve)).count() as u32;
    if approvals >= policy.min_approvals {
        return GateDecision::Land;
    }
    if round + 1 >= policy.max_rounds {
        return GateDecision::Escalate(format!(
            "quorum not reached after {} round(s): {}/{} approvals",
            round + 1, approvals, policy.min_approvals
        ));
    }
    let changes: Vec<String> = verdicts.iter().filter_map(|v| match &v.verdict {
        Verdict::Changes(m) => Some(format!("{} ({}): {}", v.role, v.agent, m)),
        Verdict::Approve => None,
    }).collect();
    GateDecision::Iterate(changes)
}
```

- [ ] **Step 4: Run, expect PASS** — `cargo test --lib gate`.

- [ ] **Step 5: Commit** — `git add src/gate.rs && git commit -m "feat(phase1): Gate quorum decision (escalates on all-flaked, never fakes a land)"`

---

### Task 7: `Conductor` — run the pipeline (hermetic integration)

**Files:**
- Create: `src/conductor.rs`
- Create: `tests/pipeline_hermetic.rs`

- [ ] **Step 1: Write the failing integration test** `tests/pipeline_hermetic.rs`:

```rust
use ensemble::*;
use std::collections::HashMap;

fn crew() -> CrewConfig {
    CrewConfig::from_toml(r#"
        [gate]
        min_approvals = 1
        max_rounds = 2
        on_flake = "exclude"
        pipeline = ["implement", "review"]
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#).unwrap()
}

fn conductor(adapters: Vec<Box<dyn Adapter>>) -> Conductor {
    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    for a in adapters { map.insert(a.name().to_string(), a); }
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
        Box::new(MockAdapter::new("codex", vec![Ok("v1".into()), Ok("v2 fixed".into())])),
        Box::new(MockAdapter::new("claude", vec![
            Ok("VERDICT: CHANGES: handle empty".into()),
            Ok("VERDICT: LGTM".into()),
        ])),
    ]);
    let out = c.run("task", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed));
    assert_eq!(out.rounds, 2);
}

#[test]
fn flaked_reviewer_is_excluded_and_escalates_not_fakes() {
    // the only reviewer flakes every round ⇒ quorum can never be met ⇒ Escalate (NOT Landed)
    let c = conductor(vec![
        Box::new(MockAdapter::new("codex", vec![Ok("impl".into()), Ok("impl".into())])),
        Box::new(MockAdapter::new("claude", vec![
            Err(AdapterError::RateLimited),
            Err(AdapterError::Empty),
        ])),
    ]);
    let out = c.run("task", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(reason) => assert!(reason.to_lowercase().contains("reviewer")),
        other => panic!("a flaked reviewer must escalate, never land: {other:?}"),
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test --test pipeline_hermetic`. Expected: compile error (Conductor undefined).

- [ ] **Step 3: Implement `src/conductor.rs`:**

```rust
use crate::adapter::Adapter;
use crate::blackboard::Blackboard;
use crate::crew::{CrewConfig, OnFlake};
use crate::gate::{decide, GateDecision, RoleVerdict};
use crate::verdict::parse_verdict;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug)]
pub enum Decision { Landed, Escalated(String) }

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
            match self.adapter_for_role(impl_role).map(|a| a.run(&impl_prompt, cwd)) {
                Some(Ok(out)) => bb.post(&out.agent, "result", &out.text),
                Some(Err(e)) => {
                    return RunOutcome { decision: Decision::Escalated(
                        format!("implementer '{impl_role}' failed: {e}")), rounds: round + 1, blackboard: bb };
                }
                None => {
                    return RunOutcome { decision: Decision::Escalated(
                        format!("no adapter for implementer role '{impl_role}'")), rounds: round + 1, blackboard: bb };
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
                        verdicts.push(RoleVerdict { role: role.to_string(), agent: out.agent, verdict: v });
                    }
                    Some(Err(e)) => {
                        // OnFlake::Exclude (the only Phase-1 policy): drop from quorum, log why.
                        let _ = OnFlake::Exclude;
                        bb.post(role, "finding", &format!("reviewer excluded — flaked: {e}"));
                    }
                    None => {
                        bb.post(role, "finding", &format!("reviewer excluded — no adapter for role '{role}'"));
                    }
                }
            }

            // 3) gate
            match decide(&verdicts, &self.crew.gate, round) {
                GateDecision::Land => return RunOutcome { decision: Decision::Landed, rounds: round + 1, blackboard: bb },
                GateDecision::Escalate(why) => return RunOutcome { decision: Decision::Escalated(why), rounds: round + 1, blackboard: bb },
                GateDecision::Iterate(changes) => { feedback = changes; }
            }
        }
        RunOutcome { decision: Decision::Escalated("max rounds reached".to_string()), rounds: max, blackboard: bb }
    }
}

/// Build an agent's prompt: the task, the inter-agent blackboard summary, and any change-requests
/// routed back to the implementer this round.
fn build_prompt(task: &str, bb: &Blackboard, feedback: &[String], role: &str) -> String {
    let mut p = format!("You are the '{role}' role on a collaborative dev crew.\nTask: {task}\n");
    let summary = bb.summary();
    if !summary.is_empty() {
        p.push_str("\n");
        p.push_str(&summary);
    }
    if !feedback.is_empty() {
        p.push_str("\nReviewers requested these changes:\n");
        for f in feedback { p.push_str(&format!("- {f}\n")); }
    }
    p
}
```

- [ ] **Step 4: Run, expect PASS** — `cargo test --test pipeline_hermetic`. Expected: 3 passed. Also run `cargo test` (all). Expected: all green.

- [ ] **Step 5: Commit** — `git add src/conductor.rs tests/pipeline_hermetic.rs && git commit -m "feat(phase1): Conductor role pipeline + blackboard routing + flake-excludes-not-fakes"`

---

### Task 8: CLI `ensemble run` + real codex adapter + `#[ignore]` live smoke

**Files:**
- Create: `src/exec_adapter.rs`
- Modify: `src/lib.rs` (add `pub mod exec_adapter;` + re-export `ExecAdapter`)
- Modify: `src/main.rs`
- Create: `tests/live_smoke.rs`

- [ ] **Step 1: Implement a generic exec adapter** `src/exec_adapter.rs` (drives a CLI by running `program [args...] <prompt>` and capturing stdout; codex is the Phase-1 instance):

```rust
use crate::adapter::{Adapter, AdapterError, AgentOutput};
use std::path::Path;
use std::process::Command;

/// Drives a vendor CLI headlessly by exec: `program <args...> <prompt>`, capturing stdout as the
/// reply. Per-vendor invocation contracts live in the constructors (design §4a). stdin is closed.
pub struct ExecAdapter {
    name: String,
    program: String,
    args: Vec<String>,
}

impl ExecAdapter {
    /// codex: `codex exec --json "<prompt>"`. (Phase-1 parses the final text from stdout; the
    /// `--json` stream is tolerated — we take the concatenated stdout. Refine parsing in Phase 1b.)
    pub fn codex() -> Self {
        Self { name: "codex".into(), program: "codex".into(),
               args: vec!["exec".into(), "--skip-git-repo-check".into()] }
    }
}

impl Adapter for ExecAdapter {
    fn name(&self) -> &str { &self.name }
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let out = Command::new(&self.program)
            .args(&self.args)
            .arg(prompt)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound =>
                return Err(AdapterError::NotInstalled(self.program.clone())),
            Err(e) => return Err(AdapterError::Flaked(e.to_string())),
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.to_lowercase().contains("rate") || err.contains("429") {
                return Err(AdapterError::RateLimited);
            }
            return Err(AdapterError::Empty);
        }
        Ok(AgentOutput { agent: self.name.clone(), text })
    }
}
```

- [ ] **Step 2: Wire the CLI** in `src/main.rs`:

```rust
use ensemble::*;
use std::collections::HashMap;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // ensemble run "<task>" [--crew <path>]
    if args.get(1).map(|s| s.as_str()) != Some("run") {
        eprintln!("usage: ensemble run \"<task>\" [--crew <crew.toml>]");
        std::process::exit(2);
    }
    let task = match args.get(2) {
        Some(t) if !t.starts_with("--") => t.clone(),
        _ => { eprintln!("usage: ensemble run \"<task>\" [--crew <crew.toml>]"); std::process::exit(2); }
    };
    let crew_path = parse_flag(&args, "--crew").unwrap_or_else(|| "crew.toml".to_string());
    let crew = match std::path::Path::new(&crew_path).exists() {
        true => CrewConfig::from_path(Path::new(&crew_path)).unwrap_or_else(|e| { eprintln!("crew config: {e}"); std::process::exit(1); }),
        false => CrewConfig::from_path(Path::new("examples/crew.toml")).unwrap_or_else(|e| { eprintln!("no crew.toml and examples/crew.toml unreadable: {e}"); std::process::exit(1); }),
    };

    // Phase-1 adapter registry: only codex is a real adapter yet.
    let mut adapters: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    adapters.insert("codex".into(), Box::new(ExecAdapter::codex()));

    let c = Conductor::new(crew, adapters);
    let out = c.run(&task, Path::new("."));
    match out.decision {
        Decision::Landed => { println!("LANDED after {} round(s)", out.rounds); }
        Decision::Escalated(why) => { eprintln!("ESCALATED after {} round(s): {}", out.rounds, why); std::process::exit(1); }
    }
}

fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}
```

- [ ] **Step 3: Add `pub mod exec_adapter;` and `pub use exec_adapter::ExecAdapter;` to `src/lib.rs`.**

- [ ] **Step 4: Write the `#[ignore]` live smoke** `tests/live_smoke.rs`:

```rust
use ensemble::*;
use std::path::Path;

#[test]
#[ignore = "live: requires a working `codex` CLI on PATH + auth"]
fn codex_exec_adapter_answers() {
    let a = ExecAdapter::codex();
    match a.run("Reply with exactly one word: PONG", Path::new(".")) {
        Ok(out) => { assert_eq!(out.agent, "codex"); assert!(out.text.to_uppercase().contains("PONG")); }
        Err(AdapterError::NotInstalled(_)) => eprintln!("codex not installed — skipping assertion"),
        Err(e) => panic!("codex live smoke failed: {e}"),
    }
}
```

- [ ] **Step 5: Run** — `cargo build` (compiles), `cargo test` (hermetic all green; the live smoke is ignored). Optionally run the live smoke manually: `cargo test --test live_smoke -- --ignored`.

- [ ] **Step 6: Commit** — `git add src/exec_adapter.rs src/main.rs src/lib.rs tests/live_smoke.rs && git commit -m "feat(phase1): ExecAdapter (codex) + ensemble run CLI + #[ignore] live smoke"`

---

## Self-Review (done)

- **Spec coverage:** §3 components all have tasks — adapter (T2), blackboard (T4), conductor (T7), gate (T6), CLI (T8); crew.toml (T5); verdict parse (T3). Inter-agent comms = blackboard summary injected in `build_prompt` (T7). Flake-degrade = exclude-not-fake (T6 `decide` empty-guard + T7 reviewer loop). Deferred (noted): real worktree isolation (Phase-1 runs in `.` / cwd — worktree-per-task is Phase 2's parallel-tasks need), `Retry`/`Substitute` on_flake (Phase 2), the claude/opencode/agy real adapters (Phase 1b — design §4a has recipes; agy needs the PTY/json-probe).
- **Type consistency:** `Adapter::run -> Result<AgentOutput, AdapterError>`, `AgentOutput{agent,text}`, `Verdict{Approve,Changes(String)}`, `RoleVerdict{role,agent,verdict}`, `GateDecision{Land,Iterate(Vec<String>),Escalate(String)}`, `Decision{Landed,Escalated(String)}`, `Conductor::new(CrewConfig, HashMap<String,Box<dyn Adapter>>)` — consistent across T2/T3/T6/T7/T8.
- **Placeholder scan:** no TBD/TODO-as-impl; every code step is complete. The `--json` codex stdout parsing is intentionally coarse for Phase 1 (noted as Phase-1b refinement) but functional.

## Deferred to later phases (explicit)
- Phase 1b: claude/opencode/agy real adapters (agy via the §4a json-probe-then-PTY recipe); structured codex `--json` parsing.
- Phase 2: real git-worktree-per-task isolation; parallel pipelines; `Retry`/`Substitute` on_flake; backup-agent wiring.
- Phase 3: cross-machine over Tailscale (design §4b).
- Phase 4: signed proofpack, blind-review anonymization, ACP/MCP.
