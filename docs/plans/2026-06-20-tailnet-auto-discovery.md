# ensemble — auto-discover agent hosts on the tailnet (default-on)

> TDD per task. Build/test via WSL (`CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`). Work on `main`. Gate codex+claude.

**Goal:** today you hand-write `[agents.<n>] node = "http://host:7878"` per agent in `crew.toml`. Make ensemble **automatically** find which tailnet machines are running `ensemble serve` and which AI CLIs each hosts, and route roles there — no hand-written URLs. Operator decisions: **discovery default-ON**; when no tailnet node hosts a needed agent, **fall back to the local CLI** (only error if absent locally too). Explicit `node =` in crew.toml still wins. A `--no-discover` flag opts out; `ensemble nodes` shows what was found.

**Architecture:** `discovery.rs` gains: `parse_health_agents` (parse `/health` JSON → agent names, hermetic), `probe_agents` (GET `<url>/health`, 2s timeout), `build_agent_hosts` (pure: (url, agents)[] → agent→url, first host wins), `discover_agent_hosts(port)` (probe every ONLINE tailnet peer's `:port/health`, compose the map). `adapters_for` resolves each agent: explicit `node` > discovered host > local adapter. New `ensemble nodes` command + `--no-discover`.

---

### Task 1: discovery probe + map primitives

**Files:** `src/discovery.rs`; `src/lib.rs` (re-export the new fns).

- [ ] **Step 1 (impl):**
```rust
use std::collections::HashMap;
use std::time::Duration;

/// Parse a node's `/health` body `{"ok":true,"agents":[...]}` into agent names. Hermetic.
pub fn parse_health_agents(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("agents")
                .and_then(|a| a.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

/// GET `<base_url>/health` and return the agents that node hosts (empty if unreachable/not serving).
pub fn probe_agents(base_url: &str) -> Vec<String> {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    match ureq::get(&url).timeout(Duration::from_secs(2)).call() {
        Ok(r) => r.into_string().map(|s| parse_health_agents(&s)).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Pure: build agent→url from (url, hosted_agents) pairs; the FIRST host of an agent wins. Hermetic.
pub fn build_agent_hosts(nodes: &[(String, Vec<String>)]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (url, agents) in nodes {
        for a in agents {
            map.entry(a.clone()).or_insert_with(|| url.clone());
        }
    }
    map
}

/// Auto-discover agent hosts on the tailnet: for each ONLINE peer, probe `http://<dns>:<port>/health`
/// and record which agents it hosts (first host wins). Empty if tailscale is absent or nobody serves
/// (→ the caller falls back to local). Uses MagicDNS names (requires MagicDNS on the tailnet).
pub fn discover_agent_hosts(port: u16) -> HashMap<String, String> {
    let nodes: Vec<(String, Vec<String>)> = discover_nodes()
        .into_iter()
        .filter(|n| n.online)
        .map(|n| {
            let url = format!("http://{}:{}", n.dns_name, port);
            let agents = probe_agents(&url);
            (url, agents)
        })
        .filter(|(_, agents)| !agents.is_empty())
        .collect();
    build_agent_hosts(&nodes)
}
```

- [ ] **Step 2 (tests):** add to `discovery.rs` tests:
```rust
    #[test]
    fn parses_health_agents_json() {
        let a = parse_health_agents(r#"{"ok":true,"agents":["codex","claude"]}"#);
        assert_eq!(a, vec!["codex", "claude"]);
        assert!(parse_health_agents("not json").is_empty());
        assert!(parse_health_agents(r#"{"ok":true}"#).is_empty());
    }

    #[test]
    fn build_agent_hosts_first_host_wins() {
        let nodes = vec![
            ("http://a:7878".to_string(), vec!["codex".to_string(), "claude".to_string()]),
            ("http://b:7878".to_string(), vec!["codex".to_string(), "agy".to_string()]),
        ];
        let m = build_agent_hosts(&nodes);
        assert_eq!(m["codex"], "http://a:7878"); // first host wins
        assert_eq!(m["claude"], "http://a:7878");
        assert_eq!(m["agy"], "http://b:7878");
    }

    #[test]
    fn probe_agents_reads_a_live_serve_health() {
        use crate::adapter::{Adapter, MockAdapter};
        let mut local: std::collections::HashMap<String, Box<dyn Adapter>> =
            std::collections::HashMap::new();
        local.insert("codex".into(), Box::new(MockAdapter::new("codex", vec![Ok("x".into())])));
        local.insert("claude".into(), Box::new(MockAdapter::new("claude", vec![Ok("y".into())])));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || crate::serve::serve_until_n(server, local, 1));
        let mut agents = probe_agents(&url);
        agents.sort();
        assert_eq!(agents, vec!["claude".to_string(), "codex".to_string()]);
        h.join().unwrap();
    }
```

- [ ] **Step 3:** `src/lib.rs` re-export: `pub use discovery::{build_agent_hosts, discover_agent_hosts, discover_nodes, parse_health_agents, probe_agents, Node};`. **Step 4:** test green; fmt; clippy. Commit `feat(discovery): probe /health + build agent->node map from the tailnet`.

---

### Task 2: wire auto-discovery into `adapters_for` + `ensemble nodes` + `--no-discover`

**Files:** `src/main.rs`.

- [ ] **Step 1 (impl):** rewrite `adapters_for` to take a `discover: bool` and resolve explicit > discovered > local. Only probe the tailnet when discovery is on AND some needed agent lacks an explicit node.
```rust
fn adapters_for(crew: &CrewConfig, discover: bool) -> HashMap<String, Box<dyn Adapter>> {
    let agents: std::collections::HashSet<&str> =
        crew.roles.values().map(|r| r.agent.as_str()).collect();
    // Probe the tailnet only if discovery is on AND at least one needed agent has no explicit node.
    let needs = discover && agents.iter().any(|a| crew.node_for(a).is_none());
    let discovered = if needs {
        ensemble::discovery::discover_agent_hosts(7878)
    } else {
        std::collections::HashMap::new()
    };
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    for agent in agents {
        if let Some(node) = crew.node_for(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node))); // explicit wins
        } else if let Some(node) = discovered.get(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node))); // auto-discovered
        } else {
            match agent {
                "codex" => { m.insert(agent.into(), Box::new(ExecAdapter::codex())); }
                "claude" => { m.insert(agent.into(), Box::new(ExecAdapter::claude())); }
                "opencode" => { m.insert(agent.into(), Box::new(ExecAdapter::opencode())); }
                "agy" => { m.insert(agent.into(), Box::new(AgyAdapter::new())); }
                _ => {}
            }
        }
    }
    m
}
```
Update the three call sites (`run_single`, `run_many`, `dispatch_cmd`) to pass `!has_flag(args, "--no-discover")`. Add:
```rust
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}
```
Add the `nodes` subcommand + USAGE line:
```rust
        Some("nodes") => nodes_cmd(&args),
```
```rust
/// `ensemble nodes` — probe the tailnet and print which agent each discovered `serve` node hosts.
fn nodes_cmd(_args: &[String]) {
    let hosts = ensemble::discovery::discover_agent_hosts(7878);
    if hosts.is_empty() {
        println!(
            "no ensemble nodes discovered (is `tailscale` installed, MagicDNS on, and are peers running `ensemble serve`?)"
        );
        return;
    }
    println!("discovered agent hosts on the tailnet:");
    let mut entries: Vec<(&String, &String)> = hosts.iter().collect();
    entries.sort();
    for (agent, url) in entries {
        println!("  {agent} -> {url}");
    }
}
```
USAGE: add `ensemble nodes   (probe the tailnet for serve hosts)` and note `--no-discover` on run/dispatch.

- [ ] **Step 2:** `cargo build` + `cargo test` green; fmt; clippy `-D warnings`; `ensemble nodes` runs (prints the no-nodes line when offline — reachability). Commit `feat(cli): default-on tailnet auto-discovery (explicit>discovered>local) + ensemble nodes + --no-discover`.

---

### Task 3: docs

**Files:** `docs/AUTONOMOUS-BACKLOG.md` (log), `docs/2026-06-19-ensemble-design.md` (note auto-discovery wired), `examples/crew.toml` (comment: nodes auto-discovered when omitted).
- [ ] Commit `docs: tailnet auto-discovery wired (no hand-written node URLs)`.

## Notes / deferred
- Uses MagicDNS names; if MagicDNS is off, fall back to Tailscale IPs (peer `TailscaleIPs`) — follow-up.
- Port fixed at 7878 (serve default); a `--port` / `[discovery] port` override = follow-up.
- Probes online peers serially with a 2s timeout each; parallelize if peer counts grow.
- Model / per-agent CLI flags config (`[agents.<n>] model=/args=`) is a SEPARATE requested feature — not in this slice.
