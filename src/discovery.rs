use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

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
    match Command::new("tailscale")
        .args(["status", "--json"])
        .output()
    {
        Ok(o) if o.status.success() => parse_tailscale_status(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Parse the `Peer` map of `tailscale status --json`. Hermetic.
pub fn parse_tailscale_status(json: &str) -> Vec<Node> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let peers = match v.get("Peer").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return Vec::new(),
    };
    peers
        .values()
        .filter_map(|p| {
            let name = p.get("HostName")?.as_str()?.to_string();
            let dns_name = p
                .get("DNSName")?
                .as_str()?
                .trim_end_matches('.')
                .to_string();
            let online = p.get("Online").and_then(|o| o.as_bool()).unwrap_or(false);
            Some(Node {
                name,
                dns_name,
                online,
            })
        })
        .collect()
}

/// Parse a node's `/health` body `{"ok":true,"agents":[...]}` into agent names. Hermetic.
pub fn parse_health_agents(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("agents").and_then(|a| a.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// GET `<base_url>/health` and return the agents that node hosts (empty if unreachable / not serving).
pub fn probe_agents(base_url: &str) -> Vec<String> {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    match ureq::get(&url).timeout(Duration::from_secs(2)).call() {
        Ok(r) => r
            .into_string()
            .map(|s| parse_health_agents(&s))
            .unwrap_or_default(),
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
            (
                "http://a:7878".to_string(),
                vec!["codex".to_string(), "claude".to_string()],
            ),
            (
                "http://b:7878".to_string(),
                vec!["codex".to_string(), "agy".to_string()],
            ),
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
        local.insert(
            "codex".into(),
            Box::new(MockAdapter::new("codex", vec![Ok("x".into())])),
        );
        local.insert(
            "claude".into(),
            Box::new(MockAdapter::new("claude", vec![Ok("y".into())])),
        );
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || crate::serve::serve_until_n(server, local, 1));
        let mut agents = probe_agents(&url);
        agents.sort();
        assert_eq!(agents, vec!["claude".to_string(), "codex".to_string()]);
        h.join().unwrap();
    }
}
