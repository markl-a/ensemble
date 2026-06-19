use crate::adapter::Adapter;
use crate::wire::{RunRequest, RunResponse};
use std::collections::HashMap;
use std::path::Path;

type Local = HashMap<String, Box<dyn Adapter>>;

/// Run the agent-host forever on `bind` (e.g. "0.0.0.0:7878"), dispatching `/run` to `local`.
pub fn serve(bind: &str, local: Local) -> std::io::Result<()> {
    let server = tiny_http::Server::http(bind)
        .map_err(|e| std::io::Error::other(format!("bind {bind}: {e}")))?;
    serve_loop(server, local, None);
    Ok(())
}

/// Serve exactly `n` requests then return (for tests).
pub fn serve_until_n(server: tiny_http::Server, local: Local, n: usize) {
    serve_loop(server, local, Some(n));
}

fn serve_loop(server: tiny_http::Server, local: Local, limit: Option<usize>) {
    let mut served = 0usize;
    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        let method = req.method().clone();
        if method == tiny_http::Method::Get && url == "/health" {
            let agents: Vec<&String> = local.keys().collect();
            let body = serde_json::json!({ "ok": true, "agents": agents }).to_string();
            let _ = req.respond(json_response(body));
        } else if method == tiny_http::Method::Post && url == "/run" {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let resp = handle_run(&local, &body);
            let _ = req.respond(json_response(
                serde_json::to_string(&resp).unwrap_or_default(),
            ));
        } else {
            let _ =
                req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
        served += 1;
        if let Some(n) = limit {
            if served >= n {
                break;
            }
        }
    }
}

fn handle_run(local: &Local, body: &str) -> RunResponse {
    let req: RunRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return RunResponse::err("?", "Flaked", &format!("bad request: {e}")),
    };
    let adapter = match local.get(&req.agent) {
        Some(a) => a,
        None => {
            return RunResponse::err(
                &req.agent,
                "NotInstalled",
                &format!("agent '{}' not on this node", req.agent),
            )
        }
    };
    match &req.repo {
        // Phase 3a: run the CLI in the node's own checkout (no git-sync).
        None => match adapter.run(&req.prompt, Path::new(".")) {
            Ok(out) => RunResponse::ok(&out.agent, &out.text),
            Err(e) => RunResponse::err(&req.agent, kind_of(&e), &e.to_string()),
        },
        // Phase 3b-1: reproduce the orchestrator's base, run there, return the edits as a bundle.
        Some(ctx) => handle_run_synced(adapter.as_ref(), &req.agent, &req.prompt, ctx),
    }
}

/// Git-synced run: materialize the orchestrator's base in a scratch repo, run the agent there,
/// commit its edits onto `dispatch/<job_id>`, and return that branch as a bundle.
fn handle_run_synced(
    adapter: &dyn Adapter,
    agent: &str,
    prompt: &str,
    ctx: &crate::wire::RepoCtx,
) -> RunResponse {
    use base64::Engine;
    if !is_safe_job_id(&ctx.job_id) {
        return RunResponse::err(agent, "Flaked", "unsafe job_id");
    }
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

/// A `job_id` becomes part of a branch name + a temp-dir name — reject anything that could escape a
/// path or break a ref (no separators, no whitespace, no leading dot).
fn is_safe_job_id(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn kind_of(e: &crate::adapter::AdapterError) -> &'static str {
    use crate::adapter::AdapterError::*;
    match e {
        Flaked(_) => "Flaked",
        Empty => "Empty",
        RateLimited => "RateLimited",
        NotInstalled(_) => "NotInstalled",
    }
}

fn json_response(body: String) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body).with_header(
        "content-type: application/json"
            .parse::<tiny_http::Header>()
            .unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Adapter, MockAdapter};
    use std::collections::HashMap;

    struct FileWriter {
        name: String,
    }
    impl Adapter for FileWriter {
        fn name(&self) -> &str {
            &self.name
        }
        fn run(
            &self,
            _p: &str,
            cwd: &std::path::Path,
        ) -> Result<crate::adapter::AgentOutput, crate::adapter::AdapterError> {
            std::fs::write(cwd.join("node_made.txt"), "NODE").unwrap();
            Ok(crate::adapter::AgentOutput {
                agent: self.name.clone(),
                text: "wrote node_made.txt".into(),
            })
        }
    }

    #[test]
    fn synced_run_executes_on_orchestrator_base_and_returns_edits() {
        use base64::Engine;
        // build an orchestrator repo + bundle its HEAD
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
        let base = crate::repo_sync::bundle_rev(repo, "HEAD").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&base);

        let mut local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        local.insert(
            "codex".into(),
            Box::new(FileWriter {
                name: "codex".into(),
            }),
        );
        let req = RunRequest {
            agent: "codex".into(),
            prompt: "make a file".into(),
            repo: Some(crate::wire::RepoCtx {
                base_bundle_b64: b64,
                base_ref: "HEAD".into(),
                job_id: "codex-test".into(),
            }),
        };
        let resp = handle_run(&local, &serde_json::to_string(&req).unwrap());
        assert!(resp.ok, "synced run should succeed: {:?}", resp.error);
        let rr = resp
            .repo_result
            .expect("a synced run returns a repo result");
        assert_eq!(rr.branch, "dispatch/codex-test");
        // the returned bundle carries node_made.txt on the dispatch branch
        let dec = base64::engine::general_purpose::STANDARD
            .decode(&rr.result_bundle_b64)
            .unwrap();
        let vtmp = tempfile::tempdir().unwrap();
        let v = vtmp.path();
        std::fs::write(v.join("b"), &dec).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(v)
            .args(["init", "-q"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(v)
            .args([
                "fetch",
                "--quiet",
                v.join("b").to_str().unwrap(),
                "dispatch/codex-test",
            ])
            .output()
            .unwrap();
        let show = std::process::Command::new("git")
            .arg("-C")
            .arg(v)
            .args(["show", "FETCH_HEAD:node_made.txt"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&show.stdout), "NODE");
    }

    #[test]
    fn serve_dispatches_run_to_local_adapter() {
        let mut local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        local.insert(
            "codex".into(),
            Box::new(MockAdapter::new("codex", vec![Ok("REMOTE-OK".into())])),
        );
        // bind ephemeral, capture the addr, serve one request in a thread
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || serve_until_n(server, local, 1));

        let a = crate::remote_adapter::RemoteAdapter::new("codex", &url);
        let out = a.run("do it", std::path::Path::new(".")).unwrap();
        assert_eq!(out.text, "REMOTE-OK");
        h.join().unwrap();
    }
}
