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
        })
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
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["branch", "-D", &self.branch])
            .output();
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
}
