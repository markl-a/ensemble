# ensemble Phase-2c — result persistence (a LANDED task's work survives)

> REQUIRED SUB-SKILL: subagent-driven-development. TDD. **Build/test via WSL** (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`) — native debug hits LNK1104. Work in `D:\Projects\ensemble` on `main`.

**Goal:** today a LANDED task's changes are thrown away — `Worktree`'s Drop does `git worktree remove` AND `git branch -D`, so even approved work vanishes. Make a LANDED run COMMIT the agents' changes and KEEP its `ensemble/<slug>` branch (worktree dir still removed); an ESCALATED run still discards everything. The operator merges the branch when ready (no auto-merge — that's a later opt-in).

**Architecture:** `Worktree` gains a `keep_branch` flag (default false → branch deleted on Drop, current behavior) + a `commit()` (capture the agents' edits) + a `keep()` (flip the flag). `Conductor::run_in_repo`, on `Decision::Landed`, commits + keeps + records the branch in the outcome. `RunOutcome` gains `branch: Option<String>`. The CLI prints the kept branch.

---

### Task 1: `Worktree` — `commit()`, `keep()`, conditional branch-delete on Drop

**Files:** `src/worktree.rs`.

- [ ] **Step 1 (test)** add to `worktree.rs` tests (the existing test helper `git()` is there):

```rust
#[test]
fn committed_and_kept_branch_survives_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);

    let branch;
    {
        let mut wt = Worktree::create(repo, "persist-me").unwrap();
        branch = wt.branch().to_string();
        std::fs::write(wt.path.join("new.txt"), "hello").unwrap();
        let committed = wt.commit("ensemble: persist-me").unwrap();
        assert!(committed, "a new untracked file should produce a commit");
        wt.keep();
    } // drop: worktree dir removed, branch KEPT

    // the branch still exists and carries new.txt
    let out = std::process::Command::new("git").arg("-C").arg(repo)
        .args(["branch", "--list", &branch]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains(&branch), "kept branch must survive drop");
    let show = std::process::Command::new("git").arg("-C").arg(repo)
        .args(["show", &format!("{branch}:new.txt")]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&show.stdout), "hello");
}

#[test]
fn unkept_branch_is_deleted_on_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);
    let branch;
    {
        let wt = Worktree::create(repo, "discard-me").unwrap();
        branch = wt.branch().to_string();
    } // drop: not kept → branch deleted
    let out = std::process::Command::new("git").arg("-C").arg(repo)
        .args(["branch", "--list", &branch]).output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&branch), "unkept branch must be deleted");
}
```

- [ ] **Step 2:** run → FAIL (commit/keep/branch don't exist).

- [ ] **Step 3 (impl):**
  - Add `keep_branch: bool` field to `Worktree`; set `keep_branch: false` in `create`.
  - Add:
```rust
impl Worktree {
    pub fn branch(&self) -> &str { &self.branch }

    /// Keep the branch after this worktree is dropped (so a LANDED result persists).
    pub fn keep(&mut self) { self.keep_branch = true; }

    /// Stage all changes in the worktree and commit them onto its branch. Returns Ok(false) when
    /// there was nothing to commit (the agents may have already committed, or produced nothing).
    pub fn commit(&self, message: &str) -> std::io::Result<bool> {
        let add = Command::new("git").arg("-C").arg(&self.path).args(["add", "-A"]).output()?;
        if !add.status.success() {
            return Err(std::io::Error::other(format!("git add: {}", String::from_utf8_lossy(&add.stderr))));
        }
        // nothing staged ⇒ nothing to commit (don't error)
        let diff = Command::new("git").arg("-C").arg(&self.path)
            .args(["diff", "--cached", "--quiet"]).status()?;
        if diff.success() { return Ok(false); } // exit 0 = no staged changes
        let out = Command::new("git").arg("-C").arg(&self.path)
            .args(["commit", "-m", message]).output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!("git commit: {}", String::from_utf8_lossy(&out.stderr))));
        }
        Ok(true)
    }
}
```
  - Drop: remove the worktree dir ALWAYS; delete the branch ONLY if `!self.keep_branch`:
```rust
impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = Command::new("git").arg("-C").arg(&self.repo)
            .args(["worktree", "remove", "--force"]).arg(&self.path).output();
        if !self.keep_branch {
            let _ = Command::new("git").arg("-C").arg(&self.repo)
                .args(["branch", "-D", &self.branch]).output();
        }
    }
}
```

- [ ] **Step 4:** run → PASS (both new tests + the existing `create_then_drop_removes_worktree`). **Step 5:** commit `feat(phase2c): Worktree commit()/keep() + keep-branch-on-drop`.

---

### Task 2: `RunOutcome.branch` + persist on LANDED in `run_in_repo`

**Files:** `src/conductor.rs`; `src/lib.rs` (RunOutcome already re-exported — no change).

- [ ] **Step 1 (impl + adjust tests):**
  - Add `pub branch: Option<String>` to `RunOutcome`.
  - Every `RunOutcome { ... }` literal in `run()` (the implementer-error, no-adapter, Land, Escalate, and max-rounds arms) gets `branch: None`. (There are ~5.)
  - `run_in_repo`'s worktree-unavailable Escalate also `branch: None`.
  - `run_in_repo` Ok arm: on `Decision::Landed`, commit + keep + record the branch:
```rust
Ok(mut wt) => {
    let mut out = self.run(task, &wt.path);
    if matches!(out.decision, Decision::Landed) {
        // Persist: capture the agents' edits and keep the branch so the work survives.
        match wt.commit(&format!("ensemble: {task}")) {
            Ok(_) => {
                wt.keep();
                out.branch = Some(wt.branch().to_string());
            }
            Err(e) => out.blackboard.post("ensemble", "finding", &format!("commit failed, work NOT persisted: {e}")),
        }
    }
    out // wt drops → worktree removed; branch kept iff we called keep()
}
```

- [ ] **Step 2 (test)** append to `tests/pipeline_hermetic.rs` — a LANDED run keeps a branch carrying the agent's file. Use an adapter that WRITES a file in cwd then approves:

```rust
struct WriterThenLgtm { name: String, file: String, content: String }
impl Adapter for WriterThenLgtm {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        if !self.file.is_empty() { std::fs::write(cwd.join(&self.file), &self.content).unwrap(); }
        Ok(AgentOutput { agent: self.name.clone(), text: if self.file.is_empty() { "VERDICT: LGTM".into() } else { format!("wrote {}", self.file) } })
    }
}

#[test]
fn landed_run_persists_work_on_a_kept_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for a in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
    }
    std::fs::write(repo.join("seed"), "x").unwrap();
    for a in [&["add","."][..], &["commit","-q","-m","init"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
    }
    let mut map: std::collections::HashMap<String, Box<dyn Adapter>> = std::collections::HashMap::new();
    map.insert("codex".into(), Box::new(WriterThenLgtm{ name:"codex".into(), file:"out.txt".into(), content:"DONE".into() }));
    map.insert("claude".into(), Box::new(WriterThenLgtm{ name:"claude".into(), file:String::new(), content:String::new() }));
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("write out.txt", repo);
    assert!(matches!(out.decision, Decision::Landed));
    let branch = out.branch.clone().expect("LANDED must record a kept branch");
    // the branch exists and carries out.txt = DONE
    let show = std::process::Command::new("git").arg("-C").arg(repo)
        .args(["show", &format!("{branch}:out.txt")]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&show.stdout), "DONE");
}
```
(`crew()` is the test helper already in the file; ensure its min_approvals/pipeline let a single LGTM land — it does: min_approvals=1, pipeline implement→review.)

- [ ] **Step 3:** run → PASS (and all existing pipeline tests still green — they don't read `.branch`). **Step 4:** commit `feat(phase2c): persist LANDED work — commit + keep branch, record in RunOutcome`.

---

### Task 3: CLI prints the kept branch

**Files:** `src/main.rs`.

- [ ] **Step 1:** in `run_single`, on `Decision::Landed`, also print the branch:
```rust
Decision::Landed => {
    print!("LANDED after {} round(s)", out.rounds);
    if let Some(b) = &out.branch {
        print!(" → work kept on branch `{b}` (merge it with: git merge {b})");
    }
    println!();
}
```
  In `run_many`, append the branch to each LANDED line similarly (`out.branch`).

- [ ] **Step 2:** `cargo build` + `cargo test` green; `cargo fmt --check`; `cargo clippy -D warnings`. **Step 3:** commit `feat(phase2c): CLI reports the kept branch on LANDED`.

---

## Notes / deferred
- No AUTO-merge into the base branch — kept-branch + operator merges. An opt-in `--merge` (antfarm ff-only-after-gate pattern, design §4b) is a later option.
- Cross-machine: a remote node's worktree/branch lives on that node (Phase 3b coordinates which branch + syncs it back).
