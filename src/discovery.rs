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
