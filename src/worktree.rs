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
    repo: PathBuf,
    /// When false (default), the branch is deleted on Drop (the work is discarded). A LANDED run
    /// flips this via `keep()` so the committed work survives after the worktree dir is removed.
    keep_branch: bool,
}

impl Worktree {
    /// `git -C <repo> worktree add -b ensemble/<slug> <repo>/.ensemble/worktrees/<slug> HEAD`,
    /// where `<slug>` = sanitized `task_id` + a unique sequence suffix (collision-proof).
    pub fn create(repo: &Path, task_id: &str) -> std::io::Result<Self> {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let slug = format!("{}-{seq}", sanitize(task_id));
        let branch = format!("ensemble/{slug}");
        let path = repo.join(".ensemble").join("worktrees").join(&slug);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "-b", &branch])
            .arg(&path)
            .arg("HEAD")
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "git worktree add: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(Self {
            path,
            branch,
            repo: repo.to_path_buf(),
            keep_branch: false,
        })
    }

    pub fn branch(&self) -> &str {
        &self.branch
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
}
