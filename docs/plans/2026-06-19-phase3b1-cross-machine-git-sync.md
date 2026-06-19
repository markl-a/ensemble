# ensemble Phase-3b-1 — cross-machine git-sync (a remote agent's edits flow back)

> **For agentic workers:** REQUIRED SUB-SKILL: TDD per task. **Build/test via WSL** (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`) — native debug hits Defender LNK1104. Work in `D:\Projects\ensemble` on `main`. Gate every task with codex+claude (`cmd //C codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check < prompt` / `cmd //C claude -p < prompt`).

**Goal:** today a role that runs on a remote node (`RemoteAdapter` → `ensemble serve`) executes its CLI in the node's *own* `.` and its file edits never come back — `serve.rs` runs in `Path::new(".")` and `remote_adapter.rs` throws `cwd` away. Make a remote agent operate on the **orchestrator's git state** and bring its edits **back**, so cross-machine collaborative dev on the same project actually works.

**Architecture (git bundles over the existing HTTP wire — no git server):**
1. `RemoteAdapter.run(prompt, cwd)` — when `cwd` is a git worktree — bundles `cwd`'s HEAD (`git bundle create`), attaches it + a unique `job_id` to the `/run` request.
2. The node materializes that base in an **isolated temp repo**, runs its CLI there, commits the agent's edits onto a `dispatch/<job_id>` branch (the agent never touches `main` — yonder's contract), bundles that branch, returns it (+ the agent's text).
3. `RemoteAdapter` fetches the returned bundle into `cwd`'s repo and **fast-forwards** `cwd`'s worktree to the dispatch tip, so the remote agent's edits appear in the orchestrator's worktree **exactly as a local agent's would**. The Adapter abstraction holds: the conductor, the Phase-2c persistence, and the reviewer can't tell local from remote — **no conductor changes needed**.

When `cwd` is NOT a git repo (e.g. `ensemble run` with no worktree), `RemoteAdapter` falls back to the Phase-3a plain run (no repo ctx) — git-sync is automatic and best-effort.

**Tech:** Rust. New dep `base64` (bundles are bytes; JSON can't carry raw bytes). New module `src/repo_sync.rs`. Full bundles both directions in slice-1 (thin `--not <base_sha>` deltas = a noted perf follow-up).

**Scope boundary (slice-1):** the common case — a remote role runs on a clean worktree (typically the implementer, first in the pipeline), its edits fast-forward back. Multi-round mixed local+remote commits / a dirty worktree at apply time (true merge instead of ff) = explicit follow-ups, not this slice. The SQLite coordination ledger = Phase 3b-2.

---

### Task 1: add the `base64` dependency

**Files:** `Cargo.toml`.

- [ ] **Step 1:** add to `[dependencies]`:
```toml
base64 = "0.22"
```
- [ ] **Step 2:** `cargo build` (WSL) succeeds. **Step 3:** commit `chore(phase3b1): add base64 dep (git bundles travel as bytes over JSON)`.

---

### Task 2: `repo_sync.rs` — the git-bundle round-trip primitives

**Files:** Create `src/repo_sync.rs`; modify `src/lib.rs` (add `pub mod repo_sync;` + re-export).

- [ ] **Step 1 (test):** create `src/repo_sync.rs` with this module + tests. The test proves the FULL round-trip with two separate temp repos and a worktree, with NO network — exactly the cross-machine flow, in-process:

```rust
//! Cross-machine git sync via git bundles (Phase 3b-1). A bundle is a self-contained, transferable
//! packfile: the orchestrator bundles its base commit, the node materializes it in an isolated repo,
//! runs the agent, commits onto a `dispatch/<job_id>` branch, and bundles that branch back. The
//! orchestrator fast-forwards its worktree to the returned tip — so a remote agent's edits land in
//! the orchestrator's worktree just like a local agent's. No git server; rides the HTTP wire.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A git bundle — raw bytes, transferable over any byte channel.
pub type Bundle = Vec<u8>;

fn git_capture(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out)
}

fn git_run(dir: &Path, args: &[&str]) -> std::io::Result<()> {
    git_capture(dir, args).map(|_| ())
}

/// The HEAD commit SHA of `repo`.
pub fn head_sha(repo: &Path) -> std::io::Result<String> {
    let out = git_capture(repo, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// True if `dir` is inside a git work tree.
pub fn is_git_worktree(dir: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// Bundle `rev` (e.g. "HEAD") of `repo` into transferable bytes (`git bundle create - <rev>`).
pub fn bundle_rev(repo: &Path, rev: &str) -> std::io::Result<Bundle> {
    let out = git_capture(repo, &["bundle", "create", "-", rev])?;
    Ok(out.stdout)
}

/// NODE side: materialize `bundle` (carrying `base_ref`, e.g. "HEAD") into a fresh repo at `dest`,
/// then create + check out `job_branch` at that base. After this the agent edits files in `dest`.
pub fn materialize_base(
    dest: &Path,
    bundle: &[u8],
    base_ref: &str,
    job_branch: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let bundle_path = dest.join(".ensemble-base.bundle");
    std::fs::write(&bundle_path, bundle)?;
    git_run(dest, &["init", "-q"])?;
    git_run(dest, &["config", "user.email", "ensemble@node"])?;
    git_run(dest, &["config", "user.name", "ensemble-node"])?;
    let bp = bundle_path.to_string_lossy().to_string();
    git_run(dest, &["fetch", "--quiet", &bp, base_ref])?;
    git_run(dest, &["checkout", "-q", "-b", job_branch, "FETCH_HEAD"])?;
    let _ = std::fs::remove_file(&bundle_path);
    Ok(())
}

/// NODE side: stage + commit everything in `dir` onto the current branch. Returns Ok(false) if there
/// was nothing to commit (the agent produced no edits).
pub fn commit_all(dir: &Path, message: &str) -> std::io::Result<bool> {
    git_run(dir, &["add", "-A"])?;
    let clean = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["diff", "--cached", "--quiet"])
        .status()?
        .success();
    if clean {
        return Ok(false);
    }
    git_run(dir, &["commit", "-q", "-m", message])?;
    Ok(true)
}

/// NODE side: bundle `branch` of `repo` into transferable bytes.
pub fn bundle_branch(repo: &Path, branch: &str) -> std::io::Result<Bundle> {
    bundle_rev(repo, branch)
}

/// ORCHESTRATOR side: fetch `branch` from `bundle` into `repo`, then fast-forward `worktree` to that
/// tip so the remote agent's edits appear in the worktree. `worktree` must be a worktree of `repo`
/// sitting at (an ancestor of) the dispatch tip — true under `run_in_repo` (the worktree is at base).
pub fn apply_result(
    repo: &Path,
    worktree: &Path,
    bundle: &[u8],
    branch: &str,
) -> std::io::Result<()> {
    let bundle_path = repo.join(".ensemble-result.bundle");
    std::fs::write(&bundle_path, bundle)?;
    let bp = bundle_path.to_string_lossy().to_string();
    let local_ref = format!("refs/ensemble/{branch}");
    git_run(repo, &["fetch", "--quiet", &bp, &format!("{branch}:{local_ref}")])?;
    let _ = std::fs::remove_file(&bundle_path);
    git_run(worktree, &["merge", "--ff-only", "--quiet", &local_ref])?;
    Ok(())
}

/// A scratch directory for a node-side job, removed on drop.
pub struct NodeJobDir {
    pub path: PathBuf,
}
impl NodeJobDir {
    pub fn new(job_id: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ensemble-node-{job_id}"));
        Self { path }
    }
}
impl Drop for NodeJobDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(repo: &Path) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed"), "base").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn bundle_roundtrip_carries_remote_edits_back() {
        // ORCH: a repo at a base commit, with a worktree sitting at base.
        let orch_tmp = tempfile::tempdir().unwrap();
        let orch = orch_tmp.path();
        init_repo(orch);
        let wt = orch.join("wt");
        git(orch, &["worktree", "add", "-q", "-b", "ensemble/x", wt.to_str().unwrap(), "HEAD"]);

        // ORCH → NODE: bundle the base.
        let base_bundle = bundle_rev(orch, "HEAD").unwrap();

        // NODE: materialize the base in its OWN temp dir, the "agent" writes a file, commit, bundle.
        let node_tmp = tempfile::tempdir().unwrap();
        let node = node_tmp.path().join("job");
        materialize_base(&node, &base_bundle, "HEAD", "dispatch/job-1").unwrap();
        std::fs::write(node.join("remote.txt"), "FROM-REMOTE").unwrap();
        assert!(commit_all(&node, "ensemble: job-1").unwrap());
        let result_bundle = bundle_branch(&node, "dispatch/job-1").unwrap();

        // NODE → ORCH: apply the result into the worktree.
        apply_result(orch, &wt, &result_bundle, "dispatch/job-1").unwrap();

        // the remote agent's file is now in the orchestrator's worktree
        assert_eq!(
            std::fs::read_to_string(wt.join("remote.txt")).unwrap(),
            "FROM-REMOTE"
        );
    }

    #[test]
    fn commit_all_reports_nothing_to_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // no new edits → Ok(false)
        assert!(!commit_all(repo, "noop").unwrap());
    }

    #[test]
    fn is_git_worktree_detects_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_git_worktree(tmp.path()));
    }
}
```

- [ ] **Step 2:** add to `src/lib.rs`: `pub mod repo_sync;` (with the other `pub mod`s) and `pub use repo_sync::{apply_result, bundle_rev, head_sha, is_git_worktree};`.
- [ ] **Step 3:** run → all three tests PASS. **Step 4:** `cargo fmt` + `cargo clippy --all-targets -- -D warnings`. **Step 5:** commit `feat(phase3b1): repo_sync — git-bundle round-trip primitives + hermetic test`.

---

### Task 3: extend the wire — `RepoCtx` (request) + repo result (response)

**Files:** `src/wire.rs`.

- [ ] **Step 1 (impl + test):** add the structs and fields. `#[serde(default, skip_serializing_if = "Option::is_none")]` keeps Phase-3a messages byte-identical (the new fields are absent when `None`).

```rust
/// Git-sync context attached to a `/run` request (Phase 3b-1). Carries the base commit as a git
/// bundle so the node can reproduce the orchestrator's state and return the agent's edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCtx {
    /// base commit bundle, base64-encoded (`git bundle create - <base_ref>`).
    pub base_bundle_b64: String,
    /// the ref the bundle's tip is recorded under (always "HEAD" in slice-1).
    pub base_ref: String,
    /// a unique id for this job; the node commits onto `dispatch/<job_id>`.
    pub job_id: String,
}

/// Git-sync result on a `/run` response: the `dispatch/<job_id>` branch the node committed the
/// agent's edits onto, as a base64 git bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoResult {
    pub result_bundle_b64: String,
    pub branch: String,
}
```

Add to `RunRequest`:
```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoCtx>,
```
Add to `RunResponse`:
```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_result: Option<RepoResult>,
```
Set `repo: None` in the `RunRequest` literal in `remote_adapter.rs` tests (and anywhere one is built) and `repo_result: None` in `RunResponse::ok`/`err`. Add a constructor:
```rust
impl RunResponse {
    pub fn ok_with_repo(agent: &str, text: &str, result_bundle_b64: String, branch: String) -> Self {
        Self {
            ok: true,
            agent: agent.into(),
            text: text.into(),
            error: None,
            error_kind: None,
            repo_result: Some(RepoResult { result_bundle_b64, branch }),
        }
    }
}
```
Update the existing `ok`/`err` to set `repo_result: None`.

- [ ] **Step 2 (test):** extend the round-trip test in `wire.rs`:
```rust
    #[test]
    fn repo_ctx_is_omitted_when_absent_and_round_trips_when_present() {
        let plain = RunRequest { agent: "codex".into(), prompt: "hi".into(), repo: None };
        let s = serde_json::to_string(&plain).unwrap();
        assert!(!s.contains("repo"), "absent repo ctx must not appear on the wire");
        let withrepo = RunRequest {
            agent: "codex".into(),
            prompt: "hi".into(),
            repo: Some(RepoCtx { base_bundle_b64: "AAA".into(), base_ref: "HEAD".into(), job_id: "codex-0".into() }),
        };
        let back: RunRequest = serde_json::from_str(&serde_json::to_string(&withrepo).unwrap()).unwrap();
        assert_eq!(back.repo.unwrap().job_id, "codex-0");
        let r = RunResponse::ok_with_repo("codex", "done", "BBB".into(), "dispatch/codex-0".into());
        let back: RunResponse = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.repo_result.unwrap().branch, "dispatch/codex-0");
    }
```
- [ ] **Step 3:** `cargo test` green (wire + the remote_adapter tests still compile with `repo: None`). **Step 4:** commit `feat(phase3b1): wire — RepoCtx/RepoResult for git-sync over /run`.

---

### Task 4: node side — `serve.rs` runs the agent on the orchestrator's base

**Files:** `src/serve.rs`.

- [ ] **Step 1 (impl):** in `handle_run`, when `req.repo` is `Some`, do the materialize → run → commit → bundle flow instead of running in `"."`. Decode/encode base64 with the engine.

```rust
use base64::Engine;

fn handle_run(local: &Local, body: &str) -> RunResponse {
    let req: RunRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return RunResponse::err("?", "Flaked", &format!("bad request: {e}")),
    };
    let adapter = match local.get(&req.agent) {
        Some(a) => a,
        None => return RunResponse::err(&req.agent, "NotInstalled", &format!("agent '{}' not on this node", req.agent)),
    };
    match &req.repo {
        None => match adapter.run(&req.prompt, Path::new(".")) {
            Ok(out) => RunResponse::ok(&out.agent, &out.text),
            Err(e) => RunResponse::err(&req.agent, kind_of(&e), &e.to_string()),
        },
        Some(ctx) => handle_run_synced(adapter.as_ref(), &req.agent, &req.prompt, ctx),
    }
}

/// Git-synced run: reproduce the orchestrator's base in a scratch repo, run the agent there, commit
/// its edits onto `dispatch/<job_id>`, and return that branch as a bundle.
fn handle_run_synced(
    adapter: &dyn crate::adapter::Adapter,
    agent: &str,
    prompt: &str,
    ctx: &crate::wire::RepoCtx,
) -> RunResponse {
    let bundle = match base64::engine::general_purpose::STANDARD.decode(&ctx.base_bundle_b64) {
        Ok(b) => b,
        Err(e) => return RunResponse::err(agent, "Flaked", &format!("bad base bundle: {e}")),
    };
    let branch = format!("dispatch/{}", ctx.job_id);
    let job = crate::repo_sync::NodeJobDir::new(&ctx.job_id);
    if let Err(e) = crate::repo_sync::materialize_base(&job.path, &bundle, &ctx.base_ref, &branch) {
        return RunResponse::err(agent, "Flaked", &format!("materialize base: {e}"));
    }
    let out = match adapter.run(prompt, &job.path) {
        Ok(o) => o,
        Err(e) => return RunResponse::err(agent, kind_of(&e), &e.to_string()),
    };
    if let Err(e) = crate::repo_sync::commit_all(&job.path, &format!("ensemble: {}", ctx.job_id)) {
        return RunResponse::err(agent, "Flaked", &format!("commit on node: {e}"));
    }
    match crate::repo_sync::bundle_branch(&job.path, &branch) {
        Ok(b) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(b);
            RunResponse::ok_with_repo(&out.agent, &out.text, b64, branch)
        }
        Err(e) => RunResponse::err(agent, "Flaked", &format!("bundle result: {e}")),
    }
    // `job` drops → scratch repo removed
}
```

- [ ] **Step 2 (test):** add a serve test using a node-side adapter that WRITES a file in its cwd (the materialized base) then replies — proving the node runs on the orchestrator's base and bundles the edits back. Put this helper + test in `serve.rs` `#[cfg(test)]`:

```rust
    struct FileWriter { name: String }
    impl Adapter for FileWriter {
        fn name(&self) -> &str { &self.name }
        fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<crate::adapter::AgentOutput, crate::adapter::AdapterError> {
            std::fs::write(cwd.join("node_made.txt"), "NODE").unwrap();
            Ok(crate::adapter::AgentOutput { agent: self.name.clone(), text: "wrote node_made.txt".into() })
        }
    }

    #[test]
    fn synced_run_executes_on_orchestrator_base_and_returns_edits() {
        use base64::Engine;
        // build an orchestrator repo + bundle its HEAD
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        for a in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
            std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
        }
        std::fs::write(repo.join("seed"), "x").unwrap();
        for a in [&["add","."][..], &["commit","-q","-m","init"]] {
            std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
        }
        let base = crate::repo_sync::bundle_rev(repo, "HEAD").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&base);

        let mut local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        local.insert("codex".into(), Box::new(FileWriter { name: "codex".into() }));
        let req = RunRequest {
            agent: "codex".into(),
            prompt: "make a file".into(),
            repo: Some(crate::wire::RepoCtx { base_bundle_b64: b64, base_ref: "HEAD".into(), job_id: "codex-test".into() }),
        };
        let resp = handle_run(&local, &serde_json::to_string(&req).unwrap());
        assert!(resp.ok, "synced run should succeed: {:?}", resp.error);
        let rr = resp.repo_result.expect("a synced run returns a repo result");
        assert_eq!(rr.branch, "dispatch/codex-test");
        // the returned bundle carries node_made.txt on the dispatch branch
        let dec = base64::engine::general_purpose::STANDARD.decode(&rr.result_bundle_b64).unwrap();
        let vtmp = tempfile::tempdir().unwrap();
        let v = vtmp.path();
        std::fs::write(v.join("b"), &dec).unwrap();
        std::process::Command::new("git").arg("-C").arg(v).args(["init","-q"]).output().unwrap();
        std::process::Command::new("git").arg("-C").arg(v).args(["fetch","--quiet", v.join("b").to_str().unwrap(), "dispatch/codex-test"]).output().unwrap();
        let show = std::process::Command::new("git").arg("-C").arg(v).args(["show","FETCH_HEAD:node_made.txt"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&show.stdout), "NODE");
    }
```

- [ ] **Step 3:** `cargo test` green. **Step 4:** `cargo fmt` + clippy. **Step 5:** commit `feat(phase3b1): serve — git-synced run materializes base, returns edits as a bundle`.

---

### Task 5: orchestrator side — `RemoteAdapter` attaches base + applies the result

**Files:** `src/remote_adapter.rs`.

- [ ] **Step 1 (impl):** give `RemoteAdapter` a process-wide job counter and make `run` git-sync-aware. When `cwd` is a git worktree: bundle HEAD, attach `RepoCtx`, and on a response carrying `repo_result`, apply it into `cwd`. Otherwise: the existing plain path.

```rust
use std::sync::atomic::{AtomicU64, Ordering};
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);
```

In `run`, replace the request construction + response handling:
```rust
fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
    // Git-sync when cwd is a worktree: ship the base so the node edits the orchestrator's state.
    let repo = if crate::repo_sync::is_git_worktree(cwd) {
        let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("{}-{seq}", self.name);
        match crate::repo_sync::bundle_rev(cwd, "HEAD") {
            Ok(b) => {
                use base64::Engine;
                Some(crate::wire::RepoCtx {
                    base_bundle_b64: base64::engine::general_purpose::STANDARD.encode(b),
                    base_ref: "HEAD".into(),
                    job_id,
                })
            }
            Err(_) => None, // can't bundle (e.g. no commits yet) → plain run
        }
    } else {
        None
    };

    let req = RunRequest { agent: self.name.clone(), prompt: prompt.to_string(), repo };
    let body = serde_json::to_string(&req).map_err(|e| AdapterError::Flaked(format!("encode: {e}")))?;
    let url = format!("{}/run", self.base_url);
    let resp = ureq::post(&url).timeout(self.timeout).set("content-type", "application/json").send_string(&body);
    match resp {
        Ok(r) => {
            let s = r.into_string().map_err(|e| AdapterError::Flaked(format!("read: {e}")))?;
            let rr: RunResponse = serde_json::from_str(&s).map_err(|e| AdapterError::Flaked(format!("decode: {e}")))?;
            if !rr.ok {
                return Err(map_kind(rr.error_kind.as_deref(), rr.error.unwrap_or_default()));
            }
            // Bring the remote agent's edits into the orchestrator's worktree.
            if let Some(res) = rr.repo_result {
                use base64::Engine;
                let bundle = base64::engine::general_purpose::STANDARD
                    .decode(&res.result_bundle_b64)
                    .map_err(|e| AdapterError::Flaked(format!("decode result bundle: {e}")))?;
                let repo_root = git_common_repo(cwd);
                crate::repo_sync::apply_result(&repo_root, cwd, &bundle, &res.branch)
                    .map_err(|e| AdapterError::Flaked(format!("apply remote edits: {e}")))?;
            }
            Ok(AgentOutput { agent: rr.agent, text: rr.text })
        }
        Err(ureq::Error::Status(429, _)) => Err(AdapterError::RateLimited),
        Err(e) => Err(AdapterError::Flaked(format!("remote {}: {e}", self.base_url))),
    }
}
```

Add a helper to resolve the repo that owns `cwd` (a linked worktree's git objects live in the main repo; `git fetch` must target it):
```rust
/// The common git dir's parent (the repo that owns `cwd`'s objects). For a linked worktree this is
/// the MAIN repo, not the worktree dir — bundles must be fetched there. Falls back to `cwd`.
fn git_common_repo(cwd: &Path) -> std::path::PathBuf {
    let out = std::process::Command::new("git")
        .arg("-C").arg(cwd)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // <repo>/.git → <repo>; a bare/worktree common dir's parent owns the objects.
            if let Some(parent) = std::path::Path::new(&p).parent() {
                return parent.to_path_buf();
            }
        }
    }
    cwd.to_path_buf()
}
```
> NOTE: `apply_result` fetches into `repo_root` then `git merge --ff-only` in `cwd` (the worktree). Fetching into the common repo makes the ref visible to the worktree. Keep `apply_result`'s two args (`repo`, `worktree`) — pass `repo_root` and `cwd`.

- [ ] **Step 2 (test):** the existing 3 remote_adapter tests run against `Path::new(".")`. The crate root (`D:\Projects\ensemble`) IS a git worktree, so `run("ping", ".")` would now try to bundle + the stub server doesn't echo a `repo_result`, so no apply happens — but bundling `.`'s HEAD is real work and the stub ignores `repo`. To keep these unit tests hermetic and fast, point them at a NON-repo temp dir so they exercise the plain path:
  - In `remote_adapter_round_trips_ok` and `remote_adapter_maps_node_error_kind`, replace `std::path::Path::new(".")` with a `tempfile::tempdir()` path (not a git repo → plain run, `repo: None`). Add `use` as needed. The `unreachable_node` test is unaffected.
  - Add a new test proving sync apply: a stub server that, on any request, replies `ok_with_repo` carrying a bundle of a `dispatch/x` branch that adds a file; assert the file lands in the worktree. (Build the bundle with `repo_sync` helpers, mirroring Task 2's test.)

```rust
    #[test]
    fn remote_adapter_applies_returned_edits_into_the_worktree() {
        use base64::Engine;
        // a repo + worktree at base
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        for a in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
            std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
        }
        std::fs::write(repo.join("seed"), "x").unwrap();
        for a in [&["add","."][..], &["commit","-q","-m","init"]] {
            std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
        }
        let wt = repo.join("wt");
        std::process::Command::new("git").arg("-C").arg(repo)
            .args(["worktree","add","-q","-b","ensemble/x", wt.to_str().unwrap(), "HEAD"]).output().unwrap();

        // a "node" builds a dispatch branch off the same base that adds remote.txt, bundled
        let node_tmp = tempfile::tempdir().unwrap();
        let node = node_tmp.path().join("job");
        let base = crate::repo_sync::bundle_rev(repo, "HEAD").unwrap();
        crate::repo_sync::materialize_base(&node, &base, "HEAD", "dispatch/codex-0").unwrap();
        std::fs::write(node.join("remote.txt"), "REMOTE").unwrap();
        crate::repo_sync::commit_all(&node, "ensemble: codex-0").unwrap();
        let result = crate::repo_sync::bundle_branch(&node, "dispatch/codex-0").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&result);

        let (url, h) = stub_server(crate::wire::RunResponse::ok_with_repo("codex", "did it", b64, "dispatch/codex-0".into()));
        let a = RemoteAdapter::new("codex", &url);
        let out = a.run("make remote.txt", &wt).unwrap();
        assert_eq!(out.text, "did it");
        assert_eq!(std::fs::read_to_string(wt.join("remote.txt")).unwrap(), "REMOTE");
        h.join().unwrap();
    }
```
(`stub_server` already replies to one request with a fixed `RunResponse` regardless of the request body — it ignores the attached `repo` ctx, which is fine: this test exercises the orchestrator's APPLY path.)

- [ ] **Step 3:** `cargo test` green; `cargo fmt`; clippy. **Step 4:** commit `feat(phase3b1): RemoteAdapter ships base + applies remote edits into the worktree`.

---

### Task 6: end-to-end hermetic test — a remote role's edits persist on the kept branch

**Files:** `tests/cross_machine.rs` (new).

- [ ] **Step 1 (test):** drive a full `Conductor::run_in_repo` where the IMPLEMENTER is a `RemoteAdapter` pointing at an in-process `serve` whose node-side adapter writes a file; the reviewer is a local always-LGTM. Assert: LANDED, the kept branch carries the remote agent's file. This proves the whole chain — base ships out, node edits, edits flow back into the worktree, Phase-2c persists them.

```rust
use ensemble::*;
use std::collections::HashMap;

struct NodeWriter { name: String, file: String, content: String }
impl Adapter for NodeWriter {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        std::fs::write(cwd.join(&self.file), &self.content).unwrap();
        Ok(AgentOutput { agent: self.name.clone(), text: format!("wrote {}", self.file) })
    }
}
struct AlwaysLgtm { name: String }
impl Adapter for AlwaysLgtm {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, _cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        Ok(AgentOutput { agent: self.name.clone(), text: "VERDICT: LGTM".into() })
    }
}

fn crew() -> CrewConfig {
    CrewConfig::from_toml(r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#).unwrap()
}

#[test]
fn remote_implementer_edits_land_and_persist() {
    // a node hosting a file-writing "codex" over an in-process ensemble serve
    let mut node: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    node.insert("codex".into(), Box::new(NodeWriter { name: "codex".into(), file: "feature.txt".into(), content: "REMOTE-FEATURE".into() }));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let h = std::thread::spawn(move || ensemble::serve::serve_until_n(server, node, 1));

    // orchestrator repo + worktree-driven conductor, implementer = RemoteAdapter(url)
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for a in [&["init","-q"][..], &["config","user.email","t@t"], &["config","user.name","t"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
    }
    std::fs::write(repo.join("seed"), "x").unwrap();
    for a in [&["add","."][..], &["commit","-q","-m","init"]] {
        std::process::Command::new("git").arg("-C").arg(repo).args(a).output().unwrap();
    }
    let mut map: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    map.insert("codex".into(), Box::new(RemoteAdapter::new("codex", &url)));
    map.insert("claude".into(), Box::new(AlwaysLgtm { name: "claude".into() }));
    let c = Conductor::new(crew(), map);

    let out = c.run_in_repo("add the feature", repo);
    assert!(matches!(out.decision, Decision::Landed), "should land: {:?}", out.decision);
    let branch = out.branch.expect("LANDED records a kept branch");
    let show = std::process::Command::new("git").arg("-C").arg(repo)
        .args(["show", &format!("{branch}:feature.txt")]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&show.stdout), "REMOTE-FEATURE", "the remote agent's edit must persist on the kept branch");
    h.join().unwrap();
}
```

- [ ] **Step 2:** run → PASS. **Step 3:** full `cargo test` + `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` all green. **Step 4:** commit `test(phase3b1): e2e — a remote role's edits flow back and persist on the kept branch`.

---

### Task 7: live smoke (#[ignore]) + docs

**Files:** `tests/live_smoke.rs` (add one `#[ignore]`); `README.md` / `docs/2026-06-19-ensemble-design.md` (note 3b-1 done).

- [ ] **Step 1:** add an `#[ignore]` live smoke that runs a real remote CLI over `serve` on another node and asserts its edit comes back (manual cross-host, like the existing live smokes). Keep it `#[ignore]` (needs two nodes).
- [ ] **Step 2:** update the design doc Phase-3 line: 3b-1 git-sync DONE (bundles over HTTP); 3b-2 = SQLite ledger.
- [ ] **Step 3:** commit `docs(phase3b1): cross-machine git-sync done — remote edits flow back`.

---

## Notes / deferred (slice-2+)
- **Thin bundles:** ship `git bundle create - <branch> --not <base_sha>` deltas instead of full history (include `base_sha` in `RepoCtx`). Full bundles are fine for small repos; deltas matter at scale.
- **True merge, not ff-only:** `apply_result` uses `--ff-only`, correct when the worktree is at base (the common slice-1 case). Multi-round mixed local+remote commits, or a dirty worktree at apply time, need a real merge + conflict policy.
- **SQLite coordination ledger (Phase 3b-2):** durable node registry + `dispatch_queue UNIQUE(task_id)` at-most-once + yonder "terminal-record = only success signal" + orphan-claim recovery. Enables a durable pull-based backlog instead of synchronous orchestrator-push.
- **Node scratch GC:** `NodeJobDir` removes its temp repo on drop; a crashed node leaks `ensemble-node-*` temp dirs — add a startup sweep.
- **Security:** the node executes an arbitrary prompt against arbitrary code under `--dangerously-bypass...`; Phase-3a's `tailscale serve` auth + the governor/flight-recorder are the controls. A malicious base bundle is just code the agent would run anyway, but validate `job_id` is branch-safe (no `../`, no spaces) before forming `dispatch/<job_id>`.
