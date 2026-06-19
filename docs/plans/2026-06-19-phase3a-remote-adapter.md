# ensemble Phase-3a — RemoteAdapter + node agent-host + discovery

> REQUIRED SUB-SKILL: subagent-driven-development. TDD. **Build/test via WSL** (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`). Work in `D:\Projects\ensemble` on `main`.

**Goal:** the orchestrator drives an AI CLI on a *different machine* over the tailnet — plugged into the existing conductor as just another `Adapter`. A node runs `ensemble serve` (a tiny HTTP agent-host exposing its local CLIs); a `RemoteAdapter` POSTs to it; `tailscale status` discovery lists the live nodes. Plain HTTP over the tailnet (WireGuard already encrypts); `tailscale serve` zero-port + TLS is a noted hardening follow-up.

**Architecture:** A tiny blocking HTTP server (`tiny_http`) + client (`ureq`, no-TLS) speaking a 2-message JSON protocol (`/health`, `/run`). `RemoteAdapter` implements `Adapter` by calling a node's `/run`; the conductor can't tell local from remote. `serve` dispatches `/run` to the node's local adapter registry. `discovery` parses `tailscale status --json`.

**Tech:** add `tiny_http = "0.12"`, `ureq = { version = "2", default-features = false }`, `serde_json = "1"`.

---

### Task 1: deps + wire protocol types

**Files:** `Cargo.toml`; Create `src/wire.rs`; `src/lib.rs` (`pub mod wire; pub use wire::{RunRequest, RunResponse};`).

- [ ] **Step 1:** Cargo.toml `[dependencies]`: `tiny_http = "0.12"`, `ureq = { version = "2", default-features = false }`, `serde_json = "1"`.

- [ ] **Step 2 (test)** in `src/wire.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn run_request_response_round_trip_json() {
        let req = RunRequest { agent: "codex".into(), prompt: "hi".into() };
        let s = serde_json::to_string(&req).unwrap();
        let back: RunRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.agent, "codex");
        let ok = RunResponse::ok("codex", "done");
        assert!(ok.ok && ok.text == "done");
        let err = RunResponse::err("agy", "Empty", "no output");
        assert!(!err.ok && err.error_kind.as_deref() == Some("Empty"));
    }
}
```

- [ ] **Step 3 (impl)** `src/wire.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Orchestrator → node: run `agent` on `prompt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub agent: String,
    pub prompt: String,
}

/// Node → orchestrator: the result of a `/run`. `ok` true ⇒ `text` is the answer; false ⇒
/// `error_kind` (one of Flaked|Empty|RateLimited|NotInstalled) + `error` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResponse {
    pub ok: bool,
    pub agent: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_kind: Option<String>,
}

impl RunResponse {
    pub fn ok(agent: &str, text: &str) -> Self {
        Self { ok: true, agent: agent.into(), text: text.into(), error: None, error_kind: None }
    }
    pub fn err(agent: &str, kind: &str, msg: &str) -> Self {
        Self { ok: false, agent: agent.into(), text: String::new(), error: Some(msg.into()), error_kind: Some(kind.into()) }
    }
}
```

- [ ] **Step 4:** `cargo test --lib wire` → PASS. **Step 5:** commit `feat(phase3a): deps + wire protocol (RunRequest/RunResponse)`.

---

### Task 2: `RemoteAdapter` (Adapter over HTTP)

**Files:** Create `src/remote_adapter.rs`; `src/lib.rs` (`pub mod remote_adapter; pub use remote_adapter::RemoteAdapter;`).

- [ ] **Step 1 (test)** — stand up a `tiny_http` stub server in a thread, point a RemoteAdapter at it:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Adapter;
    use std::io::Read;

    fn stub_server(resp: crate::wire::RunResponse) -> (String, std::thread::JoinHandle<()>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || {
            if let Some(mut req) = server.incoming_requests().next() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                let json = serde_json::to_string(&resp).unwrap();
                let r = tiny_http::Response::from_string(json)
                    .with_header("content-type: application/json".parse::<tiny_http::Header>().unwrap());
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
        assert!(matches!(a.run("x", std::path::Path::new(".")), Err(AdapterError::Empty)));
        h.join().unwrap();
    }

    #[test]
    fn remote_adapter_flakes_on_unreachable_node() {
        let a = RemoteAdapter::new("codex", "http://127.0.0.1:1"); // nothing listening
        assert!(matches!(a.run("x", std::path::Path::new(".")), Err(AdapterError::Flaked(_))));
    }
}
```

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3 (impl)** `src/remote_adapter.rs`:

```rust
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
        Self { name: name.into(), base_url: base_url.trim_end_matches('/').into(), timeout: Duration::from_secs(300) }
    }
    pub fn with_timeout(name: &str, base_url: &str, timeout: Duration) -> Self {
        Self { name: name.into(), base_url: base_url.trim_end_matches('/').into(), timeout }
    }
}

impl Adapter for RemoteAdapter {
    fn name(&self) -> &str { &self.name }

    fn run(&self, prompt: &str, _cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let req = RunRequest { agent: self.name.clone(), prompt: prompt.to_string() };
        let body = serde_json::to_string(&req).map_err(|e| AdapterError::Flaked(format!("encode: {e}")))?;
        let url = format!("{}/run", self.base_url);
        let resp = ureq::post(&url)
            .timeout(self.timeout)
            .set("content-type", "application/json")
            .send_string(&body);
        match resp {
            Ok(r) => {
                let s = r.into_string().map_err(|e| AdapterError::Flaked(format!("read: {e}")))?;
                let rr: RunResponse = serde_json::from_str(&s)
                    .map_err(|e| AdapterError::Flaked(format!("decode: {e}")))?;
                if rr.ok {
                    Ok(AgentOutput { agent: rr.agent, text: rr.text })
                } else {
                    Err(map_kind(rr.error_kind.as_deref(), rr.error.unwrap_or_default()))
                }
            }
            Err(ureq::Error::Status(429, _)) => Err(AdapterError::RateLimited),
            Err(e) => Err(AdapterError::Flaked(format!("remote {}: {e}", self.base_url))),
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
```

- [ ] **Step 4:** run → PASS (3 tests). **Step 5:** commit `feat(phase3a): RemoteAdapter (Adapter over HTTP)`.

---

### Task 3: `ensemble serve` agent-host

**Files:** Create `src/serve.rs`; `src/lib.rs` (`pub mod serve; pub use serve::serve;`).

- [ ] **Step 1 (test)** — start the host in a thread backed by a MockAdapter, hit it with a RemoteAdapter (full loopback round-trip), and a conductor with a remote agent lands:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Adapter, MockAdapter};
    use std::collections::HashMap;

    #[test]
    fn serve_dispatches_run_to_local_adapter() {
        let mut local: HashMap<String, Box<dyn Adapter>> = HashMap::new();
        local.insert("codex".into(), Box::new(MockAdapter::new("codex", vec![Ok("REMOTE-OK".into())])));
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
```

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3 (impl)** `src/serve.rs` — expose a testable `serve_until_n` (serve N requests then return) and a public `serve` (loop forever):

```rust
use crate::adapter::Adapter;
use crate::wire::{RunRequest, RunResponse};
use std::collections::HashMap;
use std::io::Read;
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
            let _ = req.respond(json_response(serde_json::to_string(&resp).unwrap_or_default()));
        } else {
            let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
        served += 1;
        if let Some(n) = limit { if served >= n { break; } }
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
        None => RunResponse::err(&req.agent, "NotInstalled", &format!("agent '{}' not on this node", req.agent)),
    }
}

fn kind_of(e: &crate::adapter::AdapterError) -> &'static str {
    use crate::adapter::AdapterError::*;
    match e { Flaked(_) => "Flaked", Empty => "Empty", RateLimited => "RateLimited", NotInstalled(_) => "NotInstalled" }
}

fn json_response(body: String) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body)
        .with_header("content-type: application/json".parse::<tiny_http::Header>().unwrap())
}
```

- [ ] **Step 4:** run → PASS. **Step 5:** commit `feat(phase3a): ensemble serve agent-host (/health, /run)`.

---

### Task 4: tailnet node discovery

**Files:** Create `src/discovery.rs`; `src/lib.rs` (`pub mod discovery; pub use discovery::{discover_nodes, Node};`).

- [ ] **Step 1 (test)** — parse a sample `tailscale status --json`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_tailscale_peers() {
        let json = r#"{ "Peer": {
            "k1": { "HostName": "acer", "DNSName": "acer.tail.ts.net.", "Online": true },
            "k2": { "HostName": "ayaneo", "DNSName": "ayaneo.tail.ts.net.", "Online": false }
        }}"#;
        let mut nodes = parse_tailscale_status(json);
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "acer");
        assert_eq!(nodes[0].dns_name, "acer.tail.ts.net"); // trailing dot trimmed
        assert!(nodes[0].online);
        assert!(!nodes[1].online);
    }
}
```

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3 (impl)** `src/discovery.rs`:

```rust
use std::process::Command;

/// A tailnet peer that may host an ensemble agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub dns_name: String, // MagicDNS name, trailing dot trimmed (use for the agent-host URL)
    pub online: bool,
}

/// Enumerate tailnet peers via `tailscale status --json`. Returns empty if tailscale is absent or
/// errors (degrade-not-crash).
pub fn discover_nodes() -> Vec<Node> {
    match Command::new("tailscale").args(["status", "--json"]).output() {
        Ok(o) if o.status.success() => parse_tailscale_status(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Parse the `Peer` map of `tailscale status --json`. Hermetic.
pub fn parse_tailscale_status(json: &str) -> Vec<Node> {
    let v: serde_json::Value = match serde_json::from_str(json) { Ok(v) => v, Err(_) => return Vec::new() };
    let peers = match v.get("Peer").and_then(|p| p.as_object()) { Some(p) => p, None => return Vec::new() };
    peers.values().filter_map(|p| {
        let name = p.get("HostName")?.as_str()?.to_string();
        let dns_name = p.get("DNSName")?.as_str()?.trim_end_matches('.').to_string();
        let online = p.get("Online").and_then(|o| o.as_bool()).unwrap_or(false);
        Some(Node { name, dns_name, online })
    }).collect()
}
```

- [ ] **Step 4:** run → PASS. **Step 5:** commit `feat(phase3a): tailnet node discovery (tailscale status --json)`.

---

### Task 5: CLI — `ensemble serve` + remote agents in crew.toml

**Files:** Modify `src/crew.rs` (add `node` to `AgentConfig`); Modify `src/main.rs` (the `serve` subcommand + remote-aware adapter registry).

- [ ] **Step 1 (test)** in `src/crew.rs` — `[agents.<n>] node = "http://..."` parses + a helper:

```rust
#[test]
fn parses_remote_agent_node_url() {
    let toml = r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 1
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
        [agents.claude]
        node = "http://acer.tail.ts.net:7878"
    "#;
    let c = CrewConfig::from_toml(toml).unwrap();
    assert_eq!(c.node_for("claude"), Some("http://acer.tail.ts.net:7878"));
    assert_eq!(c.node_for("codex"), None);
}
```

- [ ] **Step 2:** run → FAIL.

- [ ] **Step 3 (impl):**
  - `src/crew.rs`: add `#[serde(default)] pub node: Option<String>` to `AgentConfig`; `pub fn node_for(&self, agent: &str) -> Option<&str> { self.agents.get(agent).and_then(|a| a.node.as_deref()) }`.
  - `src/main.rs`:
    - New subcommand `ensemble serve [--bind <addr>]` (default `0.0.0.0:7878`): build the local 4-adapter registry (the existing `adapters()`), call `ensemble::serve(&bind, adapters())`, print "ensemble serve on <bind>". (Blocks forever.)
    - Make the registry **crew-aware** for `run`/`run-many`: replace `adapters()` with `adapters_for(&crew)` that, for each agent referenced by a role, builds a `RemoteAdapter::new(agent, node)` when `crew.node_for(agent)` is set, else the local `ExecAdapter`/`AgyAdapter`. (So a crew.toml can put `review` on a remote node.)

  Sketch:
```rust
fn adapters_for(crew: &CrewConfig) -> HashMap<String, Box<dyn Adapter>> {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    let agents: std::collections::HashSet<&str> = crew.roles.values().map(|r| r.agent.as_str()).collect();
    for agent in agents {
        if let Some(node) = crew.node_for(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node)));
        } else {
            match agent {
                "codex" => { m.insert(agent.into(), Box::new(ExecAdapter::codex())); }
                "claude" => { m.insert(agent.into(), Box::new(ExecAdapter::claude())); }
                "opencode" => { m.insert(agent.into(), Box::new(ExecAdapter::opencode())); }
                "agy" => { m.insert(agent.into(), Box::new(AgyAdapter::new())); }
                other => { m.insert(other.into(), Box::new(ExecAdapter::generic(other))); } // a generic `<other> <prompt>` exec, or skip
            }
        }
    }
    m
}
```
(If `ExecAdapter::generic` doesn't exist, just skip unknown local agents — a missing adapter already → conductor escalates cleanly. Keep it simple: only the 4 known locals + remotes.)

- [ ] **Step 4:** `cargo test` (all green), `cargo fmt --check`, `cargo clippy -D warnings`. **Step 5:** commit `feat(phase3a): ensemble serve CLI + remote agents in crew.toml`.

---

## Notes / deferred (Phase 3b+)
- **Plain HTTP over tailnet** now (WireGuard-encrypted). Hardening: front the loopback host with `tailscale serve` (zero open ports + TLS + `tailscale-user-login` identity) — wolfpack pattern.
- The remote node runs the CLI in ITS OWN cwd. **Cross-machine git sync** (the node has the repo, results come back via a `dispatch/<job>` branch + fsync terminal-record) = **Phase 3b**, plus the SQLite coordination ledger + heartbeat→suspend→recover.
- A node's agent auth is node-local (e.g. agy needs the node's desktop session authed) — operator concern, documented.
