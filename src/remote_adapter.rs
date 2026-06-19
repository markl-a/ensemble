use crate::adapter::{Adapter, AdapterError, AgentOutput};
use crate::wire::{RunRequest, RunResponse};
use std::path::Path;
use std::time::Duration;

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

    fn run(&self, prompt: &str, _cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let req = RunRequest {
            agent: self.name.clone(),
            prompt: prompt.to_string(),
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
                if rr.ok {
                    Ok(AgentOutput {
                        agent: rr.agent,
                        text: rr.text,
                    })
                } else {
                    Err(map_kind(
                        rr.error_kind.as_deref(),
                        rr.error.unwrap_or_default(),
                    ))
                }
            }
            Err(ureq::Error::Status(429, _)) => Err(AdapterError::RateLimited),
            Err(e) => Err(AdapterError::Flaked(format!(
                "remote {}: {e}",
                self.base_url
            ))),
        }
    }
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
        let (url, h) = stub_server(crate::wire::RunResponse::ok("codex", "PONG"));
        let a = RemoteAdapter::new("codex", &url);
        let out = a.run("ping", std::path::Path::new(".")).unwrap();
        assert_eq!(out.agent, "codex");
        assert_eq!(out.text, "PONG");
        h.join().unwrap();
    }

    #[test]
    fn remote_adapter_maps_node_error_kind() {
        let (url, h) = stub_server(crate::wire::RunResponse::err("agy", "Empty", "no output"));
        let a = RemoteAdapter::new("agy", &url);
        assert!(matches!(
            a.run("x", std::path::Path::new(".")),
            Err(AdapterError::Empty)
        ));
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
