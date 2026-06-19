# ensemble Phase-2 — git worktrees + parallel pipelines + Retry/Substitute on_flake

> REQUIRED SUB-SKILL: subagent-driven-development. TDD. **Build/test via WSL** (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`) — native Windows hits LNK1104. Work in `D:\Projects\ensemble` on `main`.

**Goal:** (甲) run many tasks in parallel, each isolated in its own **git worktree** so agents can't collide; and harden the flake policy with **Retry** (re-run a flaked reviewer once) and **Substitute** (fall back to a backup agent) in addition to Phase-1's Exclude.

**Architecture:** A `Worktree` RAII type wraps `git worktree add`/`remove`. The `Conductor` gains `run_in_repo(task, repo)` (create worktree → run the existing pipeline with the worktree as cwd → drop = cleanup) and `run_many(tasks, repo)` (a `std::thread::scope` thread per task; the `Adapter` trait is already `Send + Sync`). `OnFlake` gains `Retry`/`Substitute`; reviewer-flake handling consults them; backup agents come from a new `[agents.<name>] backup = "..."` config table.

---

### Task 1: `Worktree` RAII (git worktree add/remove)

**Files:** Create `src/worktree.rs`; add `pub mod worktree; pub use worktree::Worktree;` to `src/lib.rs`.

- [ ] **Step 1 (test)** in `src/worktree.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn create_then_drop_removes_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "init"]);

        let path;
        {
            let wt = Worktree::create(repo, "task-1").unwrap();
            path = wt.path.clone();
            assert!(path.exists(), "worktree dir should exist while alive");
        } // drop → removed
        assert!(!path.exists(), "worktree dir should be removed after drop");
    }
}
```

(Add `tempfile = "3"` to `[dev-dependencies]` in Cargo.toml.)

- [ ] **Step 2:** `cargo test --lib worktree` → FAIL.

- [ ] **Step 3 (impl)** `src/worktree.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

/// An isolated `git worktree` for one task. Dropping it removes the worktree + its branch, so a
/// pipeline can mutate files without colliding with other parallel tasks or the main checkout.
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    repo: PathBuf,
}

impl Worktree {
    /// `git -C <repo> worktree add -b ensemble/<task_id> <repo>/.ensemble/worktrees/<task_id> HEAD`.
    pub fn create(repo: &Path, task_id: &str) -> std::io::Result<Self> {
        let slug = sanitize(task_id);
        let branch = format!("ensemble/{slug}");
        let path = repo.join(".ensemble").join("worktrees").join(&slug);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let out = Command::new("git")
            .arg("-C").arg(repo)
            .args(["worktree", "add", "-b", &branch])
            .arg(&path)
            .arg("HEAD")
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("git worktree add: {}", String::from_utf8_lossy(&out.stderr)),
            ));
        }
        Ok(Self { path, branch, repo: repo.to_path_buf() })
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = Command::new("git").arg("-C").arg(&self.repo)
            .args(["worktree", "remove", "--force"]).arg(&self.path).output();
        let _ = Command::new("git").arg("-C").arg(&self.repo)
            .args(["branch", "-D", &self.branch]).output();
    }
}

/// Make a task id safe for a branch name + a directory name.
fn sanitize(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    out.truncate(48);
    if out.is_empty() { out.push_str("task"); }
    out
}
```

- [ ] **Step 4:** `cargo test --lib worktree` → PASS.
- [ ] **Step 5:** commit `feat(phase2): Worktree RAII (git worktree add/remove)`.

---

### Task 2: `Conductor::run_in_repo` (run the pipeline inside a worktree)

**Files:** Modify `src/conductor.rs`.

- [ ] **Step 1 (test)** append to `tests/pipeline_hermetic.rs` — a probe adapter records the cwd it ran in; assert it was the worktree, and that the worktree is gone afterward:

```rust
use std::sync::{Arc, Mutex};

struct CwdProbe { name: String, reply: String, seen: Arc<Mutex<Vec<std::path::PathBuf>>> }
impl Adapter for CwdProbe {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        self.seen.lock().unwrap().push(cwd.to_path_buf());
        Ok(AgentOutput { agent: self.name.clone(), text: self.reply.clone() })
    }
}

#[test]
fn run_in_repo_runs_inside_a_worktree_then_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for args in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    }
    std::fs::write(repo.join("f"), "x").unwrap();
    for args in [&["add","."][..], &["commit","-q","-m","init"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> = std::collections::HashMap::new();
    map.insert("codex".into(), Box::new(CwdProbe{ name:"codex".into(), reply:"impl".into(), seen: seen.clone() }));
    map.insert("claude".into(), Box::new(CwdProbe{ name:"claude".into(), reply:"VERDICT: LGTM".into(), seen: seen.clone() }));
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("add a fn", repo);
    assert!(matches!(out.decision, Decision::Landed));
    // every adapter ran inside the worktree, not the repo root
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|p| p.to_string_lossy().contains("worktrees")), "ran outside worktree: {seen:?}");
    // worktree cleaned up
    assert!(!repo.join(".ensemble/worktrees").join("add-a-fn").exists());
}
```

- [ ] **Step 2:** `cargo test --test pipeline_hermetic run_in_repo` → FAIL.

- [ ] **Step 3 (impl):** add to `impl Conductor` (reuse the existing `run`):

```rust
/// Run the pipeline for `task` in a fresh git worktree of `repo`, cleaning it up afterward.
/// Falls back to running in `repo` itself if the worktree can't be created (logged in the outcome).
pub fn run_in_repo(&self, task: &str, repo: &std::path::Path) -> RunOutcome {
    match crate::worktree::Worktree::create(repo, task) {
        Ok(wt) => {
            let out = self.run(task, &wt.path);
            out // wt drops here → cleanup
        }
        Err(e) => {
            let mut out = self.run(task, repo);
            if let crate::conductor::Decision::Landed = out.decision {
                // surface the isolation failure without masking a real land/escalate
                out.blackboard.post("ensemble", "finding", &format!("worktree unavailable, ran in repo root: {e}"));
            }
            out
        }
    }
}
```

- [ ] **Step 4:** `cargo test --test pipeline_hermetic` → PASS (all). 
- [ ] **Step 5:** commit `feat(phase2): Conductor::run_in_repo — pipeline inside a git worktree`.

---

### Task 3: `Conductor::run_many` (parallel pipelines, 甲)

**Files:** Modify `src/conductor.rs`.

- [ ] **Step 1 (test)** append to `tests/pipeline_hermetic.rs`:

```rust
#[test]
fn run_many_runs_tasks_in_parallel_each_in_its_own_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for args in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    }
    std::fs::write(repo.join("f"), "x").unwrap();
    for args in [&["add","."][..], &["commit","-q","-m","init"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    }
    // each adapter call returns Ok; reviewer always LGTM ⇒ all land
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> = std::collections::HashMap::new();
    map.insert("codex".into(), Box::new(AlwaysOk{ name:"codex".into(), reply:"impl".into() }));
    map.insert("claude".into(), Box::new(AlwaysOk{ name:"claude".into(), reply:"VERDICT: LGTM".into() }));
    let c = Conductor::new(crew(), map);

    let outs = c.run_many(&["task one".into(), "task two".into(), "task three".into()], repo);
    assert_eq!(outs.len(), 3);
    assert!(outs.iter().all(|o| matches!(o.decision, Decision::Landed)));
    // all worktrees cleaned up
    let live = std::fs::read_dir(repo.join(".ensemble/worktrees")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(live, 0, "worktrees should be cleaned up");
}

// reusable always-ok adapter (define once near the top of the test file)
struct AlwaysOk { name: String, reply: String }
impl Adapter for AlwaysOk {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, _cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        Ok(AgentOutput { agent: self.name.clone(), text: self.reply.clone() })
    }
}
```

- [ ] **Step 2:** `cargo test --test pipeline_hermetic run_many` → FAIL.

- [ ] **Step 3 (impl):** add to `impl Conductor` (use `std::thread::scope`; `&self` and the adapters are borrowed by all threads — the `Adapter` trait is `Send + Sync`, and `run`/`run_in_repo` take `&self`):

```rust
/// Run many tasks in parallel — each in its own git worktree of `repo`. Results are returned in
/// the same order as `tasks`. (甲) Bounded by the OS thread count; for Phase 2 we spawn one thread
/// per task (the task list is operator-sized, not unbounded).
pub fn run_many(&self, tasks: &[String], repo: &std::path::Path) -> Vec<RunOutcome> {
    use std::sync::Mutex;
    let results: Vec<Mutex<Option<RunOutcome>>> = (0..tasks.len()).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for (i, task) in tasks.iter().enumerate() {
            let slot = &results[i];
            s.spawn(move || {
                let out = self.run_in_repo(task, repo);
                *slot.lock().unwrap() = Some(out);
            });
        }
    });
    results.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}
```

- [ ] **Step 4:** `cargo test --test pipeline_hermetic` → PASS.
- [ ] **Step 5:** commit `feat(phase2): Conductor::run_many — parallel pipelines (甲)`.

---

### Task 4: `OnFlake::Retry` + `Substitute` + backup-agent config

**Files:** Modify `src/crew.rs` (enum + parser + `agents` table); Modify `src/conductor.rs` (reviewer-flake handling); Modify `examples/crew.toml` if needed.

- [ ] **Step 1 (test)** in `src/crew.rs` tests — accept all three on_flake values + parse `[agents.<n>] backup`:

```rust
#[test]
fn parses_all_on_flake_and_agent_backups() {
    let toml = r#"
        pipeline = ["implement", "review"]
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
    "#;
    let c = CrewConfig::from_toml(toml).unwrap();
    assert!(matches!(c.gate.on_flake, OnFlake::Substitute));
    assert_eq!(c.backup_for("claude"), Some("opencode"));
    assert_eq!(c.backup_for("codex"), None);
}
```

And in `tests/pipeline_hermetic.rs` two conductor tests (MockAdapter scripts the flake):

```rust
#[test]
fn on_flake_retry_recovers_after_one_transient_flake() {
    // reviewer flakes once then approves; on_flake=retry ⇒ the round still gets an APPROVE ⇒ Land
    let crew = CrewConfig::from_toml(r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "retry"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#).unwrap();
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> = std::collections::HashMap::new();
    map.insert("codex".into(), Box::new(MockAdapter::new("codex", vec![Ok("impl".into())])));
    map.insert("claude".into(), Box::new(MockAdapter::new("claude", vec![Err(AdapterError::RateLimited), Ok("VERDICT: LGTM".into())])));
    let out = Conductor::new(crew, map).run("t", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed), "retry must recover the transient flake");
}

#[test]
fn on_flake_substitute_uses_the_backup_agent() {
    let crew = CrewConfig::from_toml(r#"
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
    "#).unwrap();
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> = std::collections::HashMap::new();
    map.insert("codex".into(), Box::new(MockAdapter::new("codex", vec![Ok("impl".into())])));
    map.insert("claude".into(), Box::new(MockAdapter::new("claude", vec![Err(AdapterError::Empty)])));
    map.insert("opencode".into(), Box::new(MockAdapter::new("opencode", vec![Ok("VERDICT: LGTM".into())])));
    let out = Conductor::new(crew, map).run("t", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed), "substitute must fall back to the backup agent");
}
```

- [ ] **Step 2:** run those → FAIL.

- [ ] **Step 3 (impl):**
  - `src/crew.rs`: extend `enum OnFlake { Exclude, Retry, Substitute }`; `de_on_flake` accepts `"exclude"|"retry"|"substitute"`; add `#[serde(default)] pub agents: HashMap<String, AgentConfig>` to `CrewConfig` with `pub struct AgentConfig { #[serde(default)] pub backup: Option<String> }`; add `pub fn backup_for(&self, agent: &str) -> Option<&str> { self.agents.get(agent).and_then(|a| a.backup.as_deref()) }`. Re-export `AgentConfig` in lib.rs.
  - `src/conductor.rs`: factor reviewer execution into a helper that, on the primary adapter erroring, applies `self.crew.gate.on_flake`:
    - `Exclude` → log + skip (Phase-1 behavior).
    - `Retry` → call the same adapter's `run` once more; if it then succeeds, use it; else log + exclude.
    - `Substitute` → look up `backup_for(agent)`, run that adapter; if it succeeds, push its verdict (record both the role and the substitute agent); else log + exclude.
    Keep the invariant: a verdict only enters the quorum from a `Some(Ok(..))`; every other path logs + excludes. **Never fake an approval.**

  Sketch of the reviewer loop body replacement (inside `for role in self.crew.reviewer_roles()`):

```rust
let agent_name = self.crew.roles.get(role).map(|r| r.agent.clone()).unwrap_or_default();
let mut result = self.adapter_for_role(role).map(|a| a.run(&prompt, cwd));
let mut effective_agent = agent_name.clone();

if matches!(result, Some(Err(_))) {
    match self.crew.gate.on_flake {
        OnFlake::Exclude => {}
        OnFlake::Retry => {
            bb.post(role, "finding", "reviewer flaked — retrying once");
            result = self.adapter_for_role(role).map(|a| a.run(&prompt, cwd));
        }
        OnFlake::Substitute => {
            if let Some(backup) = self.crew.backup_for(&agent_name) {
                if let Some(b) = self.adapters.get(backup) {
                    bb.post(role, "finding", &format!("reviewer flaked — substituting backup '{backup}'"));
                    effective_agent = backup.to_string();
                    result = Some(b.run(&prompt, cwd));
                }
            }
        }
    }
}

match result {
    Some(Ok(out)) => {
        let v = parse_verdict(&out.text);
        bb.post(&out.agent, "verdict", &out.text);
        verdicts.push(RoleVerdict { role: role.to_string(), agent: effective_agent, verdict: v });
    }
    Some(Err(e)) => bb.post(role, "finding", &format!("reviewer excluded — flaked: {e}")),
    None => bb.post(role, "finding", &format!("reviewer excluded — no adapter for role '{role}'")),
}
```

- [ ] **Step 4:** `cargo test` → all PASS (incl. the existing flaked→escalate test still holds for `on_flake="exclude"`). `cargo fmt`, `cargo clippy --all-targets -- -D warnings`.
- [ ] **Step 5:** commit `feat(phase2): on_flake Retry + Substitute + backup-agent config`.

---

### Task 5: CLI `ensemble run-many`

**Files:** Modify `src/main.rs`.

- [ ] **Step 1:** add a `run-many` subcommand: `ensemble run-many "<task1>" "<task2>" ... [--crew <p>] [--repo <p>]`. Collect the positional tasks (everything not a `--flag`/its value), default `--repo` to `.`, call `Conductor::run_many`, print a per-task LANDED/ESCALATED summary, exit non-zero if any escalated. Keep `ensemble run` (single) using `run_in_repo("." )` so single tasks also get worktree isolation (or keep `run` for cwd-only — your call; prefer `run_in_repo` with `--repo` default `.`).

```rust
// after the existing `run` handling, add:
// if args[1] == "run-many" { collect tasks; let outs = c.run_many(&tasks, Path::new(&repo)); ... }
```

- [ ] **Step 2:** `cargo build` + `cargo test` green; `cargo fmt --check`; `cargo clippy -D warnings`.
- [ ] **Step 3:** commit `feat(phase2): ensemble run-many CLI (parallel tasks)`.

---

## Notes / deferred
- Worktree branch is `ensemble/<task-id>`; results stay on that branch (no auto-merge in Phase 2 — landing means the gate approved; an ensemble merge step is a later phase, cf. design §4b yonder ref-pinned branch + antfarm ff-only gate).
- Parallelism = one thread per task (operator-sized lists). A bounded pool is a later refinement.
- Cross-machine over Tailscale = Phase 3.
