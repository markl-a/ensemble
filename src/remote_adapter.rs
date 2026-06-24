use crate::adapter::{detect_rate_limit, Adapter, AdapterError, AgentOutput, RateLimitInfo};
use crate::wire::{RunRequest, RunResponse};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Process-wide counter making every dispatch job id unique (so two concurrent remote runs of the
/// same agent get distinct `dispatch/<agent>-<seq>` branches on the node).
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

/// An [`Adapter`] that runs its agent on a REMOTE node's `ensemble serve` agent-host over HTTP
/// (plain HTTP over the tailnet — WireGuard encrypts the link). The conductor can't tell it apart
/// from a local adapter. When `cwd` is a git worktree (Phase 3b-1), the orchestrator's base commit
/// is shipped as a bundle and the remote agent's edits are fetched back into `cwd` — so a remote
/// agent operates on the orchestrator's git state exactly like a local one. A non-repo `cwd` uses
/// the Phase-3a plain transport (the node runs in its own checkout).
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
        // and its edits flow back. Otherwise (no repo) fall back to the Phase-3a plain run. A
        // worktree that can't bundle ERRORS (sync-or-fail) rather than silently running remotely.
        let repo = self.repo_ctx(cwd)?;
        // The exact branch the node must return for git-sync — validated on the response so a
        // buggy/hostile node can't smuggle a `../`-style ref into our fetch/merge.
        let expected_branch = repo.as_ref().map(|c| format!("dispatch/{}", c.job_id));
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
                // Bring the remote agent's edits into the orchestrator's worktree — but only when we
                // actually requested git-sync, and only the exact branch we asked for.
                match (expected_branch, rr.repo_result) {
                    (Some(exp), Some(res)) => {
                        if res.branch != exp {
                            return Err(AdapterError::Flaked(format!(
                                "node returned unexpected branch '{}' (expected '{exp}')",
                                res.branch
                            )));
                        }
                        use base64::Engine;
                        let bundle = base64::engine::general_purpose::STANDARD
                            .decode(&res.result_bundle_b64)
                            .map_err(|e| {
                                AdapterError::Flaked(format!("decode result bundle: {e}"))
                            })?;
                        let repo_root = git_common_repo(cwd);
                        crate::repo_sync::apply_result(&repo_root, cwd, &bundle, &res.branch)
                            .map_err(|e| {
                                AdapterError::Flaked(format!("apply remote edits: {e}"))
                            })?;
                    }
                    // We requested sync but the node returned nothing → its edits are lost. Fail loud
                    // rather than report a clean success with no work applied.
                    (Some(exp), None) => {
                        return Err(AdapterError::Flaked(format!(
                            "git-sync requested ({exp}) but node returned no edits"
                        )));
                    }
                    // We didn't request sync (non-repo cwd) → ignore any stray repo_result.
                    (None, _) => {}
                }
                Ok(AgentOutput {
                    agent: rr.agent,
                    text: rr.text,
                })
            }
            Err(ureq::Error::Status(429, _)) => Err(AdapterError::RateLimited(RateLimitInfo {
                reason: format!("remote node {} returned HTTP 429", self.base_url),
                retry_at: None,
            })),
            Err(e) => Err(AdapterError::Flaked(format!(
                "remote {}: {e}",
                self.base_url
            ))),
        }
    }
}

impl RemoteAdapter {
    /// Build the git-sync context for a run in `cwd`: a unique job id + a bundle of `cwd`'s HEAD.
    /// `Ok(None)` when `cwd` isn't a git worktree (→ Phase-3a plain run). `Err` when `cwd` IS a
    /// worktree but its base can't be bundled — sync-or-fail: never silently run on the node's own
    /// checkout (the edits would be lost while the run still looked successful).
    fn repo_ctx(&self, cwd: &Path) -> Result<Option<crate::wire::RepoCtx>, AdapterError> {
        if !crate::repo_sync::is_git_worktree(cwd) {
            return Ok(None);
        }
        let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("{}-{seq}", self.name);
        let bundle = crate::repo_sync::bundle_rev(cwd, "HEAD")
            .map_err(|e| AdapterError::Flaked(format!("bundle base for git-sync: {e}")))?;
        use base64::Engine;
        Ok(Some(crate::wire::RepoCtx {
            base_bundle_b64: base64::engine::general_purpose::STANDARD.encode(bundle),
            base_ref: "HEAD".into(),
            job_id,
        }))
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
        // Re-parse the wire message so the remote node's reset time survives the round-trip.
        Some("RateLimited") => AdapterError::RateLimited(
            detect_rate_limit(&msg).unwrap_or(RateLimitInfo {
                reason: msg,
                retry_at: None,
            }),
        ),
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
    fn remote_adapter_rejects_a_branch_it_did_not_request() {
        // The happy apply path is covered end-to-end by tests/cross_machine.rs (a real node that
        // echoes the requested job_id). Here we assert the SECURITY behavior: when cwd is a worktree
        // (so we requested git-sync as `dispatch/<agent>-<seq>`), a node that returns ANY other
        // branch — e.g. a `../`-style injection — is rejected, and the worktree is left untouched.
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

        // the node replies with a branch we never asked for (a path-escape attempt)
        let (url, h) = stub_server(crate::wire::RunResponse::ok_with_repo(
            "codex",
            "did it",
            "AAAA".into(),
            "../../evil".into(),
        ));
        let a = RemoteAdapter::new("codex", &url);
        let err = a.run("do work", &wt).unwrap_err();
        assert!(
            matches!(err, AdapterError::Flaked(ref m) if m.contains("unexpected branch")),
            "a mismatched result branch must be rejected: {err:?}"
        );
        h.join().unwrap();
    }

    #[test]
    fn remote_adapter_flakes_on_unreachable_node() {
        let tmp = tempfile::tempdir().unwrap(); // non-repo cwd → plain path, no base bundling
        let a = RemoteAdapter::new("codex", "http://127.0.0.1:1"); // nothing listening
        assert!(matches!(
            a.run("x", tmp.path()),
            Err(AdapterError::Flaked(_))
        ));
    }
}
