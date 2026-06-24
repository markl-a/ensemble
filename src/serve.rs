use crate::adapter::Adapter;
use crate::control_plane::{ControlPlane, LocalControlPlane};
use crate::wire::{ControlPlaneRequest, ControlPlaneResponse, RunRequest, RunResponse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

type Local = HashMap<String, Box<dyn Adapter>>;
const CONTROL_TOKEN_HEADER: &str = "x-ensemble-token";

/// Where `serve` will bind. `Explicit` = user `--bind`; `Tailnet` = the node's 100.x address
/// (reachable only over the tailnet); `Loopback` = no tailnet IP, so local-only (never 0.0.0.0).
#[derive(Debug, PartialEq, Eq)]
pub enum BindAddr {
    Explicit(String),
    Tailnet(String),
    Loopback(String),
}

impl BindAddr {
    pub fn addr(&self) -> &str {
        match self {
            BindAddr::Explicit(a) | BindAddr::Tailnet(a) | BindAddr::Loopback(a) => a,
        }
    }
}

/// Decide the bind address: an explicit `--bind` wins; else the node's tailnet IPv4 (so serve is
/// reachable only over the tailnet, not the LAN/public); else loopback (local-only) — NEVER widen
/// to 0.0.0.0 implicitly.
pub fn resolve_bind(self_ips: &[String], explicit: Option<&str>, port: u16) -> BindAddr {
    if let Some(e) = explicit {
        return BindAddr::Explicit(e.to_string());
    }
    match self_ips.iter().find(|ip| ip.contains('.')) {
        Some(ipv4) => BindAddr::Tailnet(format!("{ipv4}:{port}")),
        None => BindAddr::Loopback(format!("127.0.0.1:{port}")),
    }
}

/// Run the agent-host forever on `bind` (e.g. "0.0.0.0:7878"), dispatching `/run` to `local`.
pub fn serve(bind: &str, local: Local) -> std::io::Result<()> {
    serve_with_token(bind, local, control_token_from_env())
}

/// Run the agent-host forever with an optional control-plane token.
pub fn serve_with_token(bind: &str, local: Local, token: Option<String>) -> std::io::Result<()> {
    let server = tiny_http::Server::http(bind)
        .map_err(|e| std::io::Error::other(format!("bind {bind}: {e}")))?;
    // Reclaim node-scratch dirs whose owning serve is gone — only AFTER we hold the port, so a
    // duplicate serve that fails to bind can never sweep the live server's dirs.
    let swept = crate::repo_sync::gc_node_scratch();
    if swept > 0 {
        println!("ensemble: swept {swept} stale node-scratch dir(s)");
    }
    serve_loop_with_token(server, local, None, token);
    Ok(())
}

/// Serve exactly `n` requests then return (for tests).
pub fn serve_until_n(server: tiny_http::Server, local: Local, n: usize) {
    serve_loop(server, local, Some(n));
}

/// Serve exactly `n` requests with a control-plane token requirement (for tests).
pub fn serve_until_n_with_token(
    server: tiny_http::Server,
    local: Local,
    n: usize,
    token: Option<&str>,
) {
    serve_loop_with_token(server, local, Some(n), token.map(str::to_string));
}

fn serve_loop(server: tiny_http::Server, local: Local, limit: Option<usize>) {
    serve_loop_with_token(server, local, limit, None);
}

fn serve_loop_with_token(
    server: tiny_http::Server,
    local: Local,
    limit: Option<usize>,
    token: Option<String>,
) {
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
        } else if method == tiny_http::Method::Post && url == "/control" {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let supplied = request_header(&req, CONTROL_TOKEN_HEADER);
            let resp = handle_control(&body, token.as_deref(), supplied.as_deref());
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

fn handle_control(
    body: &str,
    required_token: Option<&str>,
    supplied_token: Option<&str>,
) -> ControlPlaneResponse {
    let req: ControlPlaneRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return ControlPlaneResponse::err("BadRequest", &format!("bad request: {e}")),
    };
    if control_requires_auth(&req) && !control_authorized(required_token, supplied_token) {
        return ControlPlaneResponse::err("Unauthorized", "control mutation requires token");
    }
    let plane = LocalControlPlane::new();
    match req {
        ControlPlaneRequest::TeamStatus { repo, team } => {
            let repo = match resolve_control_repo(&repo) {
                Ok(repo) => repo,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            let session = crate::team::resolve_team_session(
                &repo,
                Some(&team),
                "remote",
                Some("remote"),
                None,
            );
            match plane.team_status(&session) {
                Ok(status) => ControlPlaneResponse::ok_status(status),
                Err(e) => ControlPlaneResponse::err("Io", &e.to_string()),
            }
        }
        ControlPlaneRequest::TeamSay {
            repo,
            team,
            from,
            kind,
            body,
        } => {
            let repo = match resolve_control_repo(&repo) {
                Ok(repo) => repo,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            let session = crate::team::resolve_team_session(
                &repo,
                Some(&team),
                "remote",
                Some("remote"),
                None,
            );
            match plane.post_team_message(&session, &from, &kind, &body) {
                Ok(next) => ControlPlaneResponse::ok_next(next),
                Err(e) => ControlPlaneResponse::err("Io", &e.to_string()),
            }
        }
        ControlPlaneRequest::TeamInbox { repo, team, since } => {
            let repo = match resolve_control_repo(&repo) {
                Ok(repo) => repo,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            let session = crate::team::resolve_team_session(
                &repo,
                Some(&team),
                "remote",
                Some("remote"),
                None,
            );
            match plane.read_team_inbox(&session, since) {
                Ok(inbox) => ControlPlaneResponse::ok_inbox(inbox),
                Err(e) => ControlPlaneResponse::err("Io", &e.to_string()),
            }
        }
        ControlPlaneRequest::Watch { repo, name, since } => {
            let repo = match resolve_control_repo(&repo) {
                Ok(repo) => repo,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            let name = match validate_feed_target(&name) {
                Ok(name) => name,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            match plane.read_stream(&repo, &name, since) {
                Ok(stream) => ControlPlaneResponse::ok_stream(stream),
                Err(e) => ControlPlaneResponse::err("Io", &e.to_string()),
            }
        }
        ControlPlaneRequest::AppendControl { repo, name, cmd } => {
            let repo = match resolve_control_repo(&repo) {
                Ok(repo) => repo,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            let name = match validate_feed_target(&name) {
                Ok(name) => name,
                Err(e) => return ControlPlaneResponse::err("BadRequest", &e),
            };
            match plane.append_control(&repo, &name, &cmd) {
                Ok(next) => ControlPlaneResponse::ok_next(next),
                Err(e) => ControlPlaneResponse::err("Io", &e.to_string()),
            }
        }
    }
}

fn control_token_from_env() -> Option<String> {
    std::env::var("ENSEMBLE_TOKEN")
        .ok()
        .filter(|t| !t.chars().any(char::is_control))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn request_header(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.to_string().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn control_requires_auth(req: &ControlPlaneRequest) -> bool {
    matches!(
        req,
        ControlPlaneRequest::TeamSay { .. } | ControlPlaneRequest::AppendControl { .. }
    )
}

fn control_authorized(required: Option<&str>, supplied: Option<&str>) -> bool {
    match required {
        Some(required) => supplied == Some(required),
        None => true,
    }
}

fn resolve_control_repo(repo: &str) -> Result<PathBuf, String> {
    let repo = repo.trim();
    if repo.is_empty() {
        return Err("repo must not be blank".to_string());
    }
    if repo.chars().any(|c| c == '\0' || c.is_control()) {
        return Err("repo must not contain control characters".to_string());
    }
    let path = PathBuf::from(repo);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| format!("resolve current dir: {e}"))?
            .join(path)
    };
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("repo `{repo}` is not accessible: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!("repo `{repo}` is not a directory"));
    }
    Ok(canonical)
}

fn validate_feed_target(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name must not be blank".to_string());
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return Err("name must not contain path separators or control characters".to_string());
    }
    if name.chars().all(|c| c == '.') {
        return Err("name must not be dot-only".to_string());
    }
    Ok(name.to_string())
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
    use crate::control_plane::ControlPlane;
    use std::collections::HashMap;

    #[test]
    fn resolve_bind_explicit_override_wins() {
        let b = resolve_bind(&["100.1.2.3".into()], Some("0.0.0.0:9999"), 7878);
        assert_eq!(b, BindAddr::Explicit("0.0.0.0:9999".into()));
    }

    #[test]
    fn resolve_bind_prefers_tailnet_ipv4() {
        let ips = vec!["fd7a:1::5".to_string(), "100.87.70.65".to_string()];
        assert_eq!(
            resolve_bind(&ips, None, 7878),
            BindAddr::Tailnet("100.87.70.65:7878".into())
        );
    }

    #[test]
    fn resolve_bind_loopback_when_no_tailnet_ip() {
        assert_eq!(
            resolve_bind(&[], None, 7878),
            BindAddr::Loopback("127.0.0.1:7878".into())
        );
    }

    #[test]
    fn resolve_bind_addr_accessor_returns_the_string() {
        assert_eq!(resolve_bind(&[], None, 7878).addr(), "127.0.0.1:7878");
    }

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

        // a NON-repo cwd → the plain Phase-3a path (no git-sync), which is what this test exercises.
        let cwd = tempfile::tempdir().unwrap();
        let a = crate::remote_adapter::RemoteAdapter::new("codex", &url);
        let out = a.run("do it", cwd.path()).unwrap();
        assert_eq!(out.text, "REMOTE-OK");
        h.join().unwrap();
    }

    #[test]
    fn serve_control_plane_round_trips_team_watch_and_control() {
        let repo = tempfile::tempdir().unwrap();
        let session =
            crate::team::resolve_team_session(repo.path(), None, "remote", Some("remote"), None);
        let local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || serve_until_n(server, local, 5));

        let remote = crate::RemoteControlPlane::new(&url);
        let next = remote
            .post_team_message(&session, "operator", "note", "hello remote")
            .unwrap();
        assert_eq!(next, 1);

        let inbox = remote.read_team_inbox(&session, 0).unwrap();
        assert_eq!(inbox.next, 1);
        assert_eq!(inbox.messages[0].body, "hello remote");

        crate::Feed::open(crate::member_stream_path(repo.path(), "run-1"))
            .append(r#"{"from":"codex","kind":"result","body":"done"}"#)
            .unwrap();
        let stream = remote.read_stream(repo.path(), "run-1", 0).unwrap();
        assert_eq!(stream.len(), 1);
        assert!(stream[0].contains("done"));

        let control_next = remote
            .append_control(
                repo.path(),
                "run-1",
                &crate::ControlCmd::Steer {
                    from: "operator".into(),
                    prompt: "focus".into(),
                },
            )
            .unwrap();
        assert_eq!(control_next, 1);
        let control = crate::Feed::open(crate::member_control_path(repo.path(), "run-1"))
            .read_since(0)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<crate::ControlCmd>(&control[0]).unwrap(),
            crate::ControlCmd::Steer {
                from: "operator".into(),
                prompt: "focus".into()
            }
        );

        let status = remote.team_status(&session).unwrap();
        assert_eq!(status.board_len, 1);
        assert_eq!(status.streams, vec!["run-1".to_string()]);
        assert_eq!(status.controls, vec!["run-1".to_string()]);
        h.join().unwrap();
    }

    #[test]
    fn control_route_requires_token_for_mutations_when_configured() {
        let repo = tempfile::tempdir().unwrap();
        let local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h =
            std::thread::spawn(move || serve_until_n_with_token(server, local, 2, Some("secret")));

        let remote_without_token = crate::RemoteControlPlane::new(&url);
        let err = remote_without_token
            .append_control(
                repo.path(),
                "run-1",
                &crate::ControlCmd::Abort {
                    from: "operator".into(),
                    hard: true,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("Unauthorized"));

        let remote_with_token = crate::RemoteControlPlane::with_token(&url, "secret");
        let next = remote_with_token
            .append_control(
                repo.path(),
                "run-1",
                &crate::ControlCmd::Abort {
                    from: "operator".into(),
                    hard: true,
                },
            )
            .unwrap();
        assert_eq!(next, 1);
        h.join().unwrap();
    }

    #[test]
    fn control_route_allows_read_only_without_token_when_configured() {
        let repo = tempfile::tempdir().unwrap();
        crate::Feed::open(crate::member_stream_path(repo.path(), "run-1"))
            .append(r#"{"from":"codex","kind":"result","body":"done"}"#)
            .unwrap();
        let local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h =
            std::thread::spawn(move || serve_until_n_with_token(server, local, 1, Some("secret")));

        let remote_without_token = crate::RemoteControlPlane::new(&url);
        let stream = remote_without_token
            .read_stream(repo.path(), "run-1", 0)
            .unwrap();
        assert_eq!(stream.len(), 1);
        assert!(stream[0].contains("done"));
        h.join().unwrap();
    }

    #[test]
    fn control_route_rejects_unsafe_feed_targets() {
        let repo = tempfile::tempdir().unwrap();
        let req = crate::wire::ControlPlaneRequest::Watch {
            repo: repo.path().to_string_lossy().to_string(),
            name: "../run".into(),
            since: 0,
        };
        let resp = handle_control(&serde_json::to_string(&req).unwrap(), None, None);

        assert!(!resp.ok);
        assert_eq!(resp.error_kind.as_deref(), Some("BadRequest"));
        assert!(resp
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("path separators"));
    }
}
