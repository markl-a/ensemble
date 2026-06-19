//! Cross-machine git sync via git bundles (Phase 3b-1). A bundle is a self-contained, transferable
//! packfile: the orchestrator bundles its base commit, the node materializes it in an isolated repo,
//! runs the agent, commits onto a `dispatch/<job_id>` branch, and bundles that branch back. The
//! orchestrator fast-forwards its worktree to the returned tip — so a remote agent's edits land in
//! the orchestrator's worktree just like a local agent's. No git server; rides the HTTP wire.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A git bundle — raw bytes, transferable over any byte channel.
pub type Bundle = Vec<u8>;

/// Process-wide counters making scratch/staging paths unique even when two callers pass the same
/// logical id (e.g. two orchestrators sending `job_id = "codex-0"` to one node).
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);
static NODE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Make a ref/id safe to embed in a filesystem path (a branch like `dispatch/codex-0` contains `/`,
/// which is a path separator). Non `[A-Za-z0-9_-]` → `-`.
fn sanitize_ref(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// A file removed on drop — so a staged bundle never leaks, even on an early `?` return.
struct TempFile {
    path: PathBuf,
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

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
    // Stage in a unique temp file (NOT a fixed path in the repo): concurrent `run_many` applies must
    // not clobber each other's bundle between write and fetch, and it must never leak into the
    // worktree. The TempFile guard removes it on every return path.
    let seq = STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stage = TempFile {
        path: std::env::temp_dir().join(format!(
            "ensemble-stage-{}-{}-{seq}.bundle",
            sanitize_ref(branch),
            std::process::id()
        )),
    };
    std::fs::write(&stage.path, bundle)?;
    let bp = stage.path.to_string_lossy().to_string();
    let local_ref = format!("refs/ensemble/{branch}");
    git_run(
        repo,
        &["fetch", "--quiet", &bp, &format!("{branch}:{local_ref}")],
    )?;
    git_run(worktree, &["merge", "--ff-only", "--quiet", &local_ref])?;
    Ok(())
    // `stage` drops → staged bundle removed
}

/// A scratch directory for a node-side job, removed on drop.
pub struct NodeJobDir {
    pub path: PathBuf,
}
impl NodeJobDir {
    pub fn new(job_id: &str) -> Self {
        // Unique per node PROCESS, not just per job_id: two different orchestrators can each send
        // `job_id = "codex-0"`, and a deterministic dir would let one job's Drop delete the other's
        // scratch repo mid-run. pid + a local counter keep them isolated.
        let seq = NODE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ensemble-node-{}-{}-{seq}",
            sanitize_ref(job_id),
            std::process::id()
        ));
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
        git(
            orch,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "ensemble/x",
                wt.to_str().unwrap(),
                "HEAD",
            ],
        );

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
