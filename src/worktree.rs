use fs2::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide monotonic counter making every worktree slug unique, so two tasks with the same
/// (or same-after-sanitize/truncate) text can NEVER collide on a branch/path — which under
/// `run_many` would otherwise make one `git worktree add` fail and (pre-fix) silently share the
/// repo root. Uniqueness here is what makes parallel isolation correct.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// An isolated `git worktree` for one task. Dropping it removes the worktree + its branch, so a
/// pipeline can mutate files without colliding with other parallel tasks or the main checkout.
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// The unique slug (`sanitize(task_id)-<seq>`) — the branch is `ensemble/<slug>` and the per-run
    /// journal is `.ensemble/runs/<slug>.jsonl`.
    slug: String,
    repo: PathBuf,
    /// When false (default), the branch is deleted on Drop (the work is discarded). A LANDED run
    /// flips this via `keep()` so the committed work survives after the worktree dir is removed.
    keep_branch: bool,
}

impl Worktree {
    /// `git -C <repo> worktree add -b ensemble/<slug> <repo>/.ensemble/worktrees/<slug> HEAD`,
    /// where `<slug>` = sanitized `task_id` + a unique sequence suffix. The process-local
    /// sequence is only a starting point; kept branches from prior processes are skipped so a
    /// fixed governed task can be rerun.
    pub fn create(repo: &Path, task_id: &str) -> std::io::Result<Self> {
        let stem = sanitize(task_id);
        for _ in 0..1024 {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let slug = format!("{stem}-{seq}");
            let branch = format!("ensemble/{slug}");
            let path = repo.join(".ensemble").join("worktrees").join(&slug);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if path.exists() || branch_exists(repo, &branch)? {
                continue;
            }
            let out = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["worktree", "add", "-b", &branch])
                .arg(&path)
                .arg("HEAD")
                .output()?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if stderr.contains("already exists") {
                    continue;
                }
                return Err(std::io::Error::other(format!("git worktree add: {stderr}")));
            }
            return Ok(Self {
                path,
                branch,
                slug,
                repo: repo.to_path_buf(),
                keep_branch: false,
            });
        }
        Err(std::io::Error::other(
            "git worktree add: exhausted available worktree slugs",
        ))
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// The unique slug for this run (matches the `ensemble/<slug>` branch and the journal filename).
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// Keep the branch after this worktree is dropped (so a LANDED result persists).
    pub fn keep(&mut self) {
        self.keep_branch = true;
    }

    /// Stage all changes in the worktree and commit them onto its branch. Returns Ok(false) when
    /// there was nothing to commit (the agents may have already committed, or produced nothing).
    pub fn commit(&self, message: &str) -> std::io::Result<bool> {
        let add = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["add", "-A"])
            .output()?;
        if !add.status.success() {
            return Err(std::io::Error::other(format!(
                "git add: {}",
                String::from_utf8_lossy(&add.stderr)
            )));
        }
        // nothing staged ⇒ nothing to commit (don't error)
        let diff = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["diff", "--cached", "--quiet"])
            .status()?;
        if diff.success() {
            return Ok(false); // exit 0 = no staged changes
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["commit", "-m", message])
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "git commit: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(true)
    }
}

fn branch_exists(repo: &Path, branch: &str) -> std::io::Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--quiet", "--verify"])
        .arg(format!("refs/heads/{branch}"))
        .output()?;
    Ok(out.status.success())
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        if !self.keep_branch {
            let _ = Command::new("git")
                .arg("-C")
                .arg(&self.repo)
                .args(["branch", "-D", &self.branch])
                .output();
        }
    }
}

/// A git worktree that PERSISTS on disk — a live crew member's long-lived workspace, created via the
/// MCP `ensemble_worktree` tool. UNLIKE [`Worktree`] (the conductor's RAII per-task isolation, removed
/// on Drop), a `KeptWorktree` is a plain handle with NO Drop: the member keeps editing it across many
/// tool calls and even `ensemble mcp` restarts, then lands it with `ensemble merge`.
#[derive(Debug, Clone)]
pub struct KeptWorktree {
    pub path: PathBuf,
    pub branch: String,
    pub slug: String,
}

/// Create — or idempotently RE-ATTACH to — `member`'s persistent worktree for `task`. The location is
/// DETERMINISTIC (NOT the process-local SEQ that `Worktree::create` uses), so a repeat call (same
/// process, or a restarted `ensemble mcp`) returns the SAME workspace instead of colliding. `member`
/// and `task` are SEPARATE, sanitized path components — `.ensemble/worktrees/<member>/<task>`, branch
/// `ensemble/<member>/<task>` — so they can never bleed into one another (a `-` join would make
/// `(member="a", task="b-c")` and `(member="a-b", task="c")` collide, since `sanitize` itself emits
/// `-`). Different members therefore never collide; the same member asking twice for the same task
/// re-attaches. Creation is serialized by a per-repo exclusive lock (cross-thread AND cross-process),
/// so concurrent same-slug requests can't double-create or race the checks. If the target path exists
/// but is NOT a registered worktree (a stale dir left by a manual delete), this errors rather than
/// guessing — the operator clears it (`git worktree prune`).
pub fn ensure_kept_worktree(
    repo: &Path,
    member: &str,
    task: &str,
) -> std::io::Result<KeptWorktree> {
    let member = sanitize(member);
    let task = sanitize(task);

    // Serialize creation so concurrent same-slug requests (the MCP server runs each request on its own
    // thread) can't race the registered/exists checks into a double-create or a spurious "stale dir"
    // error. The lock is anchored to the repo's COMMON git dir (the shared `.git`), which every linked
    // worktree reports identically — so two `ensemble mcp` processes pointed at DIFFERENT worktree
    // roots of the same repo still serialize on ONE lock file (the target path is always under the
    // main worktree, so the lock must be repo-global, not ctx.repo-relative). fs2 is an OS advisory
    // lock → serializes across threads AND processes; auto-released if a holder dies.
    let lock_path = git_common_dir(repo)?.join("ensemble-worktree.lock");
    // append (not write) so there's no ambiguous truncate behavior — we never write to the lock file,
    // it's only a lock target (mirrors board.rs's lock-bearing open).
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&lock_path)?;
    lock.lock_exclusive()?;
    let r = ensure_locked(repo, &member, &task);
    let _ = lock.unlock();
    r
}

/// The repo's common git directory (the shared `.git`), resolved to an absolute path. Every linked
/// worktree of the same repo reports the SAME common dir, so it is a stable per-repo lock anchor
/// regardless of which worktree `repo` points at. (`--git-common-dir` may be relative when `repo` is
/// the main worktree — resolve it against `repo` then.) Reused by the MCP `ensemble_merge` lock.
pub(crate) fn git_common_dir(repo: &Path) -> std::io::Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-common-dir"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git rev-parse --git-common-dir: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(if p.is_absolute() { p } else { repo.join(p) })
}

/// The create-or-reattach decision, run while holding the per-repo worktree lock so each check below
/// is free of TOCTOU against a concurrent sibling.
fn ensure_locked(repo: &Path, member: &str, task: &str) -> std::io::Result<KeptWorktree> {
    let slug = format!("{member}/{task}");
    let canonical_branch = format!("ensemble/{slug}");
    let entries = list_worktrees(repo)?;
    // git lists the MAIN worktree first; build the target path from git's OWN normalized root so the
    // equality check below uses the same path format git emits (no suffix false-positives, no
    // cross-platform separator / macOS /private canonicalization mismatch).
    let main = entries.first().ok_or_else(|| {
        std::io::Error::other("git worktree list returned no worktrees (not a git repo?)")
    })?;
    let abs = Path::new(&main.path)
        .join(".ensemble")
        .join("worktrees")
        .join(member)
        .join(task);

    // Match a registered worktree at our target by LEXICAL path equality. Rust's `Path` comparison is
    // COMPONENT-WISE — it treats `/` and `\` as equivalent on Windows (git emits `/`, our `join` may
    // produce `\`) — and it does NOT resolve symlinks. Not resolving symlinks is REQUIRED: canonicalize
    // would make a symlink/junction squatting the target resolve to, and falsely match, the main
    // worktree or another member's workspace (re-attaching the caller to the wrong branch). Both
    // operands derive from git's output (`abs` is built from git's own main-worktree path), so their
    // casing is already consistent — no case-folding needed. A symlink squatting `abs` therefore does
    // NOT match here and is caught as an unregistered path by the `abs.exists()` guard below.
    if let Some(e) = entries.iter().find(|e| Path::new(&e.path) == abs) {
        if e.prunable || !abs.exists() {
            // git flagged the registration broken, or the dir was removed out from under git — surface
            // it instead of returning a phantom path. (Recoverable: prune, then a recreate reuses the
            // surviving branch.)
            return Err(std::io::Error::other(format!(
                "{} has a stale/prunable worktree registration; run `git worktree prune`",
                abs.display()
            )));
        }
        // re-attach, reporting the branch git ACTUALLY has checked out (not a reconstruction), so a
        // worktree a member switched to another branch isn't misreported. A DETACHED worktree has no
        // `branch` porcelain line (→ None) and thus no branch to land — fabricating the canonical name
        // would mislead a later `ensemble merge` into a ref that lacks the member's detached commits,
        // so surface the state instead of guessing.
        return match e.branch.as_deref().and_then(|b| b.strip_prefix("refs/heads/")) {
            Some(b) => Ok(KeptWorktree { path: abs, branch: b.to_string(), slug }),
            None => Err(std::io::Error::other(format!(
                "{} is in detached-HEAD state (no branch to land); check out a branch there or run `git worktree prune`",
                abs.display()
            ))),
        };
    }
    if abs.exists() {
        // a non-worktree dir — OR a symlink/junction — squatting the target path: git has no
        // registration here, so refuse rather than write into whatever it points at.
        return Err(std::io::Error::other(format!(
            "{} exists but is not a registered git worktree; remove it or run `git worktree prune`",
            abs.display()
        )));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The canonical branch may ALREADY exist — e.g. a prior worktree was deleted and the operator ran
    // `git worktree prune`, which drops the registration but KEEPS the branch (carrying the member's
    // committed work). REUSE it (`git worktree add <path> <branch>`) so that recovery actually works
    // and restores the work, rather than always passing `-b` (which fails "a branch named … already
    // exists"). Only a genuinely-new slug takes the `-b … HEAD` fresh-branch path.
    let branch_exists = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{canonical_branch}"),
        ])
        .output()?
        .status
        .success();
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(["worktree", "add"]);
    if branch_exists {
        cmd.arg(&abs).arg(&canonical_branch); // attach the surviving branch (keeps its commits)
    } else {
        cmd.args(["-b", &canonical_branch]).arg(&abs).arg("HEAD"); // fresh branch at HEAD
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git worktree add: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(KeptWorktree {
        path: abs,
        branch: canonical_branch,
        slug,
    })
}

/// One entry from `git worktree list --porcelain`: its absolute path, the branch it has checked out
/// (`refs/heads/...`, or None when detached), and whether git flagged it `prunable` (its working dir
/// is gone).
struct WtEntry {
    path: String,
    branch: Option<String>,
    prunable: bool,
}

/// Parse `git worktree list --porcelain`, run under the repo. Blocks are separated by blank lines;
/// each begins with a `worktree <path>` line and may carry `branch <ref>` and/or `prunable <reason>`.
fn list_worktrees(repo: &Path) -> std::io::Result<Vec<WtEntry>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git worktree list: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(parse_worktrees(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_worktrees(porcelain: &str) -> Vec<WtEntry> {
    let mut entries: Vec<WtEntry> = Vec::new();
    for line in porcelain.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            entries.push(WtEntry {
                path: p.trim().to_string(),
                branch: None,
                prunable: false,
            });
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(e) = entries.last_mut() {
                e.branch = Some(b.trim().to_string());
            }
        } else if line.starts_with("prunable") {
            if let Some(e) = entries.last_mut() {
                e.prunable = true;
            }
        }
    }
    entries
}

/// Make a task id safe for a branch name + a directory name.
fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(48);
    if out.is_empty() {
        out.push_str("task");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
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
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["branch", "--list", &branch])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&branch),
            "kept branch must survive drop"
        );
        let show = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["show", &format!("{branch}:new.txt")])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&show.stdout), "hello");
    }

    #[test]
    fn create_skips_existing_kept_branches_for_reruns() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let first_slug;
        {
            let mut wt = Worktree::create(repo, "repeatable task").unwrap();
            first_slug = wt.slug().to_string();
            wt.keep();
        }

        let (stem, seq) = first_slug.rsplit_once('-').unwrap();
        let seq: u64 = seq.parse().unwrap();
        for offset in 1..=16 {
            let branch = format!("ensemble/{stem}-{}", seq + offset);
            git(repo, &["branch", &branch, "HEAD"]);
        }

        let wt = Worktree::create(repo, "repeatable task").unwrap();
        assert!(
            wt.slug().starts_with(&format!("{stem}-")),
            "rerun should keep the same task stem, got {}",
            wt.slug()
        );
        assert!(
            !((seq + 1)..=(seq + 16)).any(|n| wt.slug() == format!("{stem}-{n}")),
            "rerun reused a pre-existing kept branch slug: {}",
            wt.slug()
        );
        assert!(wt.path.exists(), "rerun worktree should be created");
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
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["branch", "--list", &branch])
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains(&branch),
            "unkept branch must be deleted"
        );
    }

    fn init_repo(repo: &std::path::Path) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn kept_worktree_persists_on_disk_and_reattaches_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let w1 = ensure_kept_worktree(repo, "codex", "feature-x").unwrap();
        assert!(w1.path.exists(), "the kept worktree dir exists");
        assert_eq!(
            w1.branch, "ensemble/codex/feature-x",
            "branch carries the member as a component"
        );
        assert_eq!(w1.slug, "codex/feature-x");
        // a KeptWorktree has NO Drop removal: the dir is STILL there after the handle is dropped.
        let p1 = w1.path.clone();
        drop(w1);
        assert!(
            p1.exists(),
            "a persistent worktree must NOT be removed on drop"
        );

        // idempotent re-attach: same member+task → same path/branch, exactly one worktree registered.
        let w2 = ensure_kept_worktree(repo, "codex", "feature-x").unwrap();
        assert_eq!(w2.path, p1);
        assert_eq!(w2.branch, "ensemble/codex/feature-x");
        let list = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let n = String::from_utf8_lossy(&list.stdout)
            .lines()
            .filter(|l| l.starts_with("worktree ") && l.contains("codex/feature-x"))
            .count();
        assert_eq!(n, 1, "re-attach must not create a second worktree");
    }

    #[test]
    fn kept_worktree_errors_when_path_is_a_squatting_non_worktree_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // a plain (non-worktree) directory squatting the target path
        let squat = repo
            .join(".ensemble")
            .join("worktrees")
            .join("codex")
            .join("work");
        std::fs::create_dir_all(&squat).unwrap();
        let err = ensure_kept_worktree(repo, "codex", "work").unwrap_err();
        assert!(
            err.to_string().contains("not a registered git worktree"),
            "got: {err}"
        );
    }

    #[test]
    fn kept_worktree_is_not_confused_with_the_main_worktree() {
        // Regression (codex gate, slice 3a): matching only the final path component made the MAIN
        // worktree (the repo root) look "registered" when the repo dir name coincided with a slug,
        // so ensure_kept_worktree returned a NON-EXISTENT path. We now match the full
        // .ensemble/worktrees/<member>/<task> tail, so the main worktree can never match — even here,
        // where the repo dir basename equals what a naive flat slug "<member>-<task>" would be.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("alice-work");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let w = ensure_kept_worktree(&repo, "alice", "work").unwrap();
        assert!(
            w.path.exists(),
            "must create a REAL worktree, not false-positive on the main one"
        );
        assert!(
            w.path.ends_with(
                Path::new(".ensemble")
                    .join("worktrees")
                    .join("alice")
                    .join("work")
            ),
            "lands under .ensemble/worktrees/alice/work, got {:?}",
            w.path
        );
    }

    #[test]
    fn reattach_reports_the_branch_git_actually_has_checked_out() {
        // Regression (codex gate, slice 3a r2): re-attach must read the ACTUAL branch from git, not
        // reconstruct ensemble/<member>/<task> — else a worktree switched to another branch is
        // misreported.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let w = ensure_kept_worktree(repo, "codex", "feat").unwrap();
        assert_eq!(w.branch, "ensemble/codex/feat");
        // the member switches the worktree to a different branch
        git(&w.path, &["switch", "-c", "spike"]);
        let again = ensure_kept_worktree(repo, "codex", "feat").unwrap();
        assert_eq!(
            again.branch, "spike",
            "re-attach reflects the real checked-out branch"
        );
    }

    #[test]
    fn a_prunable_registration_errors_instead_of_returning_a_phantom_path() {
        // Regression (codex gate, slice 3a r2): if the worktree dir is deleted out from under git, the
        // registration is prunable and its path no longer exists — re-attaching to it would hand back
        // a nonexistent path. Surface it instead.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let w = ensure_kept_worktree(repo, "codex", "gone").unwrap();
        std::fs::remove_dir_all(&w.path).unwrap(); // manual delete → git marks it prunable
        let err = ensure_kept_worktree(repo, "codex", "gone").unwrap_err();
        assert!(
            err.to_string().contains("stale/prunable") || err.to_string().contains("prune"),
            "got: {err}"
        );
    }

    #[test]
    fn reattach_to_a_detached_worktree_errors_instead_of_fabricating_a_branch() {
        // Regression (codex gate, slice 3a r3): a detached worktree has no porcelain `branch` line, so
        // reconstructing ensemble/<member>/<task> would misreport a branch that lacks the detached
        // commits. Surface the detached state instead.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let w = ensure_kept_worktree(repo, "codex", "det").unwrap();
        git(&w.path, &["switch", "--detach", "HEAD"]);
        let err = ensure_kept_worktree(repo, "codex", "det").unwrap_err();
        assert!(err.to_string().contains("detached"), "got: {err}");
    }

    #[test]
    fn after_pruning_a_stale_worktree_recreate_reuses_the_branch_and_restores_work() {
        // Regression (codex gate, slice 3a r4): the prunable error tells the operator to run
        // `git worktree prune`, but prune leaves the branch behind — so recreate must REUSE it
        // (`git worktree add <path> <branch>`), not fail on `-b`. This is what makes the documented
        // recovery actually work, and it restores the member's committed work.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let w = ensure_kept_worktree(repo, "codex", "redo").unwrap();
        std::fs::write(w.path.join("work.txt"), "wip").unwrap();
        git(&w.path, &["add", "."]);
        git(&w.path, &["commit", "-q", "-m", "wip"]);
        // dir deleted out from under git, then the operator prunes the stale registration
        std::fs::remove_dir_all(&w.path).unwrap();
        git(repo, &["worktree", "prune"]);
        // recreate must SUCCEED by reusing the surviving branch, restoring the committed work
        let again = ensure_kept_worktree(repo, "codex", "redo").unwrap();
        assert_eq!(again.branch, "ensemble/codex/redo");
        assert!(
            again.path.join("work.txt").exists(),
            "the surviving branch's work is restored"
        );
    }

    #[test]
    fn the_lock_anchor_is_shared_across_linked_worktrees() {
        // Regression (codex gate, slice 3a r5): the creation lock must be keyed on the repo's COMMON
        // git dir, not ctx.repo — else a process at the main worktree and one at a linked worktree
        // lock different files yet write the same target. All worktrees must resolve to one anchor.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let w = ensure_kept_worktree(repo, "codex", "shared").unwrap();
        let from_main = std::fs::canonicalize(git_common_dir(repo).unwrap()).unwrap();
        let from_linked = std::fs::canonicalize(git_common_dir(&w.path).unwrap()).unwrap();
        assert_eq!(
            from_main, from_linked,
            "every worktree shares one lock anchor (the common .git)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_squatting_the_target_is_not_mistaken_for_a_registered_worktree() {
        // Regression (codex gate, slice 3a r7): the match must NOT resolve symlinks. Else a symlink at
        // the target resolving to the MAIN worktree would canonicalize-equal main's registration and
        // falsely "re-attach" the caller to main (returning branch=main). Lexical comparison rejects
        // it; abs.exists() (which follows the link) then flags it as an unregistered squatter.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let parent = repo.join(".ensemble").join("worktrees").join("codex");
        std::fs::create_dir_all(&parent).unwrap();
        std::os::unix::fs::symlink(repo, parent.join("evil")).unwrap(); // target → main worktree
        let err = ensure_kept_worktree(repo, "codex", "evil").unwrap_err();
        assert!(
            err.to_string().contains("not a registered git worktree"),
            "got: {err}"
        );
    }

    #[test]
    fn concurrent_same_member_task_calls_create_exactly_one_worktree() {
        // Regression (codex gate, slice 3a): without the per-repo lock, concurrent same-(member,task)
        // requests raced the registered/exists checks → double-create or a spurious "stale dir" error.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        init_repo(&repo);
        let results = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..8 {
                let repo = repo.clone();
                let results = &results;
                s.spawn(move || {
                    let r = ensure_kept_worktree(&repo, "m", "t").map(|w| w.path);
                    results.lock().unwrap_or_else(|e| e.into_inner()).push(r);
                });
            }
        });
        let paths = results.into_inner().unwrap_or_else(|e| e.into_inner());
        assert!(
            paths.iter().all(|r| r.is_ok()),
            "no concurrent call errors: {paths:?}"
        );
        let first = paths[0].as_ref().unwrap().clone();
        assert!(
            paths.iter().all(|r| r.as_ref().unwrap() == &first),
            "all re-attach to one path"
        );
        // git worktree list shows the main worktree + EXACTLY one kept worktree (no double-create).
        let list = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let n = String::from_utf8_lossy(&list.stdout)
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count();
        assert_eq!(n, 2, "main + exactly one kept worktree");
    }
}
