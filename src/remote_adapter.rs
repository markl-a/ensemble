use crate::adapter::{Adapter, AdapterError, AgentOutput};
use crate::wire::{RunRequest, RunResponse};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Process-wide counter making every dispatch job id unique (so two concurrent remote runs of the
/// same agent get distinct `dispatch/<agent>-<seq>` branches on the node).
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

/// An [`Adapter`] that runs its agent on a REMOTE node's `ensemble serve` agent-host over HTTP
/// (plain HTTP over the tailnet — WireGuard encrypts the link). The conductor can't tell it apart
/// from a local adapter. The orchestrator's `cwd` is not sent: the remote node runs the CLI in its
/// OWN checkout (cross-machine git sync = Phase 3b); Phase 3a proves the transport.
pub struct RemoteAdapter {
    name: String,
    base_url: String,
    timeout: Duration,
}

impl RemoteAdapter {
    pub fn new(name: &str, base_url: &str) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.trim_end_matches('/').into(),
            timeout: Duration::from_secs(300),
        }
    }
    pub fn with_timeout(name: &str, base_url: &str, timeout: Duration) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.trim_end_matches('/').into(),
            timeout,
        }
    }
}

impl Adapter for RemoteAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        // Git-sync when cwd is a worktree: ship the base so the node edits the orchestrator's state
        // and its edits flow back. Otherwise (no repo) fall back to the Phase-3a plain run.
        let repo = self.repo_ctx(cwd);
        let req = RunRequest {
            agent: self.name.clone(),
            prompt: prompt.to_string(),
            repo,
        };
        let body = serde_json::to_string(&req)
            .map_err(|e| AdapterError::Flaked(format!("encode: {e}")))?;
        let url = format!("{}/run", self.base_url);
        let resp = ureq::post(&url)
            .timeout(self.timeout)
            .set("content-type", "application/json")
            .send_string(&body);
        match resp {
            Ok(r) => {
                let s = r
                    .into_string()
                    .map_err(|e| AdapterError::Flaked(format!("read: {e}")))?;
                let rr: RunResponse = serde_json::from_str(&s)
                    .map_err(|e| AdapterError::Flaked(format!("decode: {e}")))?;
                if !rr.ok {
                    return Err(map_kind(
                        rr.error_kind.as_deref(),
                        rr.error.unwrap_or_default(),
                    ));
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
                Ok(AgentOutput {
                    agent: rr.agent,
                    text: rr.text,
                })
            }
            Err(ureq::Error::Status(429, _)) => Err(AdapterError::RateLimited),
            Err(e) => Err(AdapterError::Flaked(format!(
                "remote {}: {e}",
                self.base_url
            ))),
        }
    }
}

impl RemoteAdapter {
    /// Build the git-sync context for a run in `cwd`: a unique job id + a bundle of `cwd`'s HEAD.
    /// Returns `None` when `cwd` isn't a git worktree or HEAD can't be bundled (e.g. no commits yet)
    /// — the node then runs in its own checkout (Phase 3a behavior).
    fn repo_ctx(&self, cwd: &Path) -> Option<crate::wire::RepoCtx> {
        if !crate::repo_sync::is_git_worktree(cwd) {
            return None;
        }
        let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("{}-{seq}", self.name);
        let bundle = crate::repo_sync::bundle_rev(cwd, "HEAD").ok()?;
        use base64::Engine;
        Some(crate::wire::RepoCtx {
            base_bundle_b64: base64::engine::general_purpose::STANDARD.encode(bundle),
            base_ref: "HEAD".into(),
            job_id,
        })
    }
}

/// The repo that owns `cwd`'s git objects. For a linked worktree this is the MAIN repo, not the
/// worktree dir — a result bundle must be fetched THERE for the ref to be visible to the worktree.
/// Falls back to `cwd` if git can't resolve it.
fn git_common_repo(cwd: &Path) -> std::path::PathBuf {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // <repo>/.git → <repo> (the dir that owns the object store).
            if let Some(parent) = std::path::Path::new(&p).parent() {
                if !parent.as_os_str().is_empty() {
                    return parent.to_path_buf();
                }
            }
        }
    }
    cwd.to_path_buf()
}

fn map_kind(kind: Option<&str>, msg: String) -> AdapterError {
    match kind {
        Some("Empty") => AdapterError::Empty,
        Some("RateLimited") => AdapterError::RateLimited,
        Some("NotInstalled") => AdapterError::NotInstalled(msg),
        _ => AdapterError::Flaked(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Adapter;

    fn stub_server(resp: crate::wire::RunResponse) -> (String, std::thread::JoinHandle<()>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || {
            if let Some(mut req) = server.incoming_requests().next() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                let json = serde_json::to_string(&resp).unwrap();
                let r = tiny_http::Response::from_string(json).with_header(
                    "content-type: application/json"
                        .parse::<tiny_http::Header>()
                        .unwrap(),
                );
                req.respond(r).unwrap();
            }
        });
        (url, h)
    }

    #[test]
    fn remote_adapter_round_trips_ok() {
        // run in a NON-repo dir → plain Phase-3a path (no git-sync), so the stub's fixed reply applies.
        let tmp = tempfile::tempdir().unwrap();
        let (url, h) = stub_server(crate::wire::RunResponse::ok("codex", "PONG"));
        let a = RemoteAdapter::new("codex", &url);
        let out = a.run("ping", tmp.path()).unwrap();
        assert_eq!(out.agent, "codex");
        assert_eq!(out.text, "PONG");
        h.join().unwrap();
    }

    #[test]
    fn remote_adapter_maps_node_error_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let (url, h) = stub_server(crate::wire::RunResponse::err("agy", "Empty", "no output"));
        let a = RemoteAdapter::new("agy", &url);
        assert!(matches!(a.run("x", tmp.path()), Err(AdapterError::Empty)));
        h.join().unwrap();
    }

    #[test]
    fn remote_adapter_applies_returned_edits_into_the_worktree() {
        use base64::Engine;
        // a repo + worktree at base
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
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
        let wt = repo.join("wt");
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                "ensemble/x",
                wt.to_str().unwrap(),
                "HEAD",
            ])
            .output()
            .unwrap();

        // a "node" builds a dispatch branch off the same base that adds remote.txt, bundled
        let node_tmp = tempfile::tempdir().unwrap();
        let node = node_tmp.path().join("job");
        let base = crate::repo_sync::bundle_rev(repo, "HEAD").unwrap();
        crate::repo_sync::materialize_base(&node, &base, "HEAD", "dispatch/codex-0").unwrap();
        std::fs::write(node.join("remote.txt"), "REMOTE").unwrap();
        crate::repo_sync::commit_all(&node, "ensemble: codex-0").unwrap();
        let result = crate::repo_sync::bundle_branch(&node, "dispatch/codex-0").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&result);

        let (url, h) = stub_server(crate::wire::RunResponse::ok_with_repo(
            "codex",
            "did it",
            b64,
            "dispatch/codex-0".into(),
        ));
        let a = RemoteAdapter::new("codex", &url);
        let out = a.run("make remote.txt", &wt).unwrap();
        assert_eq!(out.text, "did it");
        assert_eq!(
            std::fs::read_to_string(wt.join("remote.txt")).unwrap(),
            "REMOTE"
        );
        h.join().unwrap();
    }

    #[test]
    fn remote_adapter_flakes_on_unreachable_node() {
        let a = RemoteAdapter::new("codex", "http://127.0.0.1:1"); // nothing listening
        assert!(matches!(
            a.run("x", std::path::Path::new(".")),
            Err(AdapterError::Flaked(_))
        ));
    }
}
