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
    match local.get(&req.agent) {
        Some(a) => match a.run(&req.prompt, Path::new(".")) {
            Ok(out) => RunResponse::ok(&out.agent, &out.text),
            Err(e) => RunResponse::err(&req.agent, kind_of(&e), &e.to_string()),
        },
        None => RunResponse::err(
            &req.agent,
            "NotInstalled",
            &format!("agent '{}' not on this node", req.agent),
        ),
    }
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
