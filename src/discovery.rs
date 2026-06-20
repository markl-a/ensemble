use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A tailnet peer that may host an ensemble agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub dns_name: String, // MagicDNS name, trailing dot trimmed (use for the agent-host URL)
    pub tailscale_ips: Vec<String>, // stable 100.x / fd7a:: addrs — the MagicDNS-off fallback
    pub online: bool,
}

impl Node {
    /// The host to address this peer at: the MagicDNS name when present, otherwise the stable
    /// TailscaleIP (so discovery still works with MagicDNS off — e.g. a second VPN mangling DNS).
    /// IPv4 (100.x) preferred over IPv6. `None` when the peer has neither (unusable → skip).
    pub fn endpoint(&self) -> Option<String> {
        if !self.dns_name.is_empty() {
            return Some(self.dns_name.clone());
        }
        self.tailscale_ips
            .iter()
            .find(|ip| ip.contains('.')) // prefer IPv4 (100.x)
            .or_else(|| self.tailscale_ips.first())
            .cloned()
    }
}

/// How long to wait for `tailscale status --json` before giving up. Discovery is a DEFAULT-ON hot
/// path (every `run`/`agent`/`nodes`), so a wedged tailscaled must never hang the whole tool.
const STATUS_TIMEOUT: Duration = Duration::from_secs(4);

/// Run `tailscale status --json`, bounded by a timeout so a wedged tailscaled can't hang the
/// default-on discovery path. Returns the captured stdout, or `None` if it can't spawn / times out.
/// Safe to hard-kill on timeout because `tailscale status` is a single well-behaved child that
/// writes bounded JSON then exits — unlike the test gate's arbitrary user command, which may spawn a
/// process tree (see test_gate.rs for why a hard timeout there needs process-group/job-object teardown).
fn capture_bounded(mut cmd: Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain stdout on a worker thread so a full pipe buffer can't deadlock, and so the main thread
    // can give up on a wedged child after `timeout`.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf); // recv may be gone (we timed out) — that's fine.
    });
    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            // Got all of stdout (EOF). Reap WITHOUT an unbounded wait — poll briefly, then kill if it
            // somehow lingers (a well-behaved `tailscale status` has already exited). Only a SUCCESSFUL
            // exit feeds discovery — restores the old exit-status gate so a non-zero run never
            // contributes hosts, even if it printed JSON-ish bytes.
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return status.success().then_some(buf),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return None;
                    }
                }
            }
        }
        Err(_) => {
            // Wedged past the timeout: kill the child (closing the pipe unblocks the reader) and give up.
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Enumerate tailnet peers via `tailscale status --json`. Returns empty if tailscale is absent,
/// errors, or wedges past the timeout (degrade-not-crash).
pub fn discover_nodes() -> Vec<Node> {
    let mut c = Command::new("tailscale");
    c.args(["status", "--json"]);
    match capture_bounded(c, STATUS_TIMEOUT) {
        Some(out) => parse_tailscale_status(&out),
        None => Vec::new(),
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
            // DNSName may be absent/empty when MagicDNS is off — still a usable peer via its IP.
            let dns_name = p
                .get("DNSName")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .trim_end_matches('.')
                .to_string();
            let tailscale_ips: Vec<String> = p
                .get("TailscaleIPs")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let online = p.get("Online").and_then(|o| o.as_bool()).unwrap_or(false);
            Some(Node {
                name,
                dns_name,
                tailscale_ips,
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
    // Bound the CONNECT, not just the overall request: ureq's request `.timeout()` does not bound the
    // TCP connect, so a peer that silently drops :7878 (an idle iOS/Android device, a firewall) hangs
    // ~30s — and since `ensemble nodes` is gated on the SLOWEST parallel probe, one such peer made the
    // whole discovery take ~30s on a real multi-device tailnet. `timeout_connect` caps each probe.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(800))
        .timeout(Duration::from_secs(2))
        .build();
    match agent.get(&url).call() {
        Ok(r) => r
            .into_string()
            .map(|s| parse_health_agents(&s))
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Probe every online peer's `<endpoint>:<port>/health` IN PARALLEL, returning `(url, hosted_agents)`
/// per reachable node (order preserved → deterministic first-host-wins). `probe` is injected for
/// hermetic testing. A serial probe would wait `2s × N` on a fleet with offline-but-listed peers.
fn probe_all<F>(online: &[Node], port: u16, probe: F) -> Vec<(String, Vec<String>)>
where
    F: Fn(&str) -> Vec<String> + Sync,
{
    let probe = &probe;
    std::thread::scope(|s| {
        let handles: Vec<_> = online
            .iter()
            .map(|n| {
                s.spawn(move || {
                    let host = n.endpoint()?;
                    // Bracket IPv6 literals so the address colons don't corrupt the host:port authority.
                    let url = if host.contains(':') {
                        format!("http://[{host}]:{port}")
                    } else {
                        format!("http://{host}:{port}")
                    };
                    let agents = probe(&url);
                    Some((url, agents))
                })
            })
            .collect();
        // Join in spawn order → first-host-wins stays deterministic.
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    })
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

/// Auto-discover agent hosts on the tailnet: for each ONLINE peer, probe `http://<endpoint>:<port>/health`
/// (in parallel) and record which agents it hosts (first host wins). Empty if tailscale is absent or
/// nobody serves (→ the caller falls back to local). Uses MagicDNS names, falling back to TailscaleIPs.
pub fn discover_agent_hosts(port: u16) -> HashMap<String, String> {
    let online: Vec<Node> = discover_nodes().into_iter().filter(|n| n.online).collect();
    let nodes: Vec<(String, Vec<String>)> = probe_all(&online, port, probe_agents)
        .into_iter()
        .filter(|(_, agents)| !agents.is_empty())
        .collect();
    build_agent_hosts(&nodes)
}

/// Parse this node's own TailscaleIPs out of `tailscale status --json` (`Self.TailscaleIPs`).
/// Empty when logged out / unparsable. Hermetic.
pub fn parse_self_ips(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("Self")
                .and_then(|s| s.get("TailscaleIPs"))
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// This node's tailnet IPs (empty if tailscale is absent/logged out or wedges past the timeout).
pub fn self_tailscale_ips() -> Vec<String> {
    let mut c = Command::new("tailscale");
    c.args(["status", "--json"]);
    match capture_bounded(c, STATUS_TIMEOUT) {
        Some(out) => parse_self_ips(&out),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_self_ips_reads_self_tailscale_ips() {
        let json = r#"{ "Self": { "HostName": "yoyogood",
            "TailscaleIPs": ["100.87.70.65", "fd7a:1::5"] }, "Peer": {} }"#;
        assert_eq!(parse_self_ips(json), vec!["100.87.70.65", "fd7a:1::5"]);
    }

    #[test]
    fn parse_self_ips_empty_when_logged_out() {
        let json = r#"{ "Self": { "HostName": "yoyogood", "TailscaleIPs": null }, "Peer": {} }"#;
        assert!(parse_self_ips(json).is_empty());
        assert!(parse_self_ips("not json").is_empty());
    }

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
    fn parse_captures_tailscale_ips() {
        let json = r#"{ "Peer": {
            "k": { "HostName": "ayaneo", "DNSName": "ayaneo.tail.ts.net.", "Online": true,
                   "TailscaleIPs": ["100.107.205.98", "fd7a:1::1"] }
        }}"#;
        let nodes = parse_tailscale_status(json);
        assert_eq!(nodes[0].tailscale_ips, vec!["100.107.205.98", "fd7a:1::1"]);
    }

    #[test]
    fn endpoint_prefers_magicdns_name() {
        let n = Node {
            name: "m".into(),
            dns_name: "m.tail.ts.net".into(),
            tailscale_ips: vec!["100.1.2.3".into()],
            online: true,
        };
        assert_eq!(n.endpoint().as_deref(), Some("m.tail.ts.net"));
    }

    #[test]
    fn endpoint_falls_back_to_ipv4_when_no_magicdns() {
        // MagicDNS off (or a VPN mangling DNS): no DNSName → address by the stable 100.x IP, IPv4 first.
        let n = Node {
            name: "m".into(),
            dns_name: "".into(),
            tailscale_ips: vec!["fd7a:1::1".into(), "100.9.9.9".into()],
            online: true,
        };
        assert_eq!(n.endpoint().as_deref(), Some("100.9.9.9"));
    }

    #[test]
    fn endpoint_none_when_no_dns_and_no_ip() {
        let n = Node {
            name: "m".into(),
            dns_name: "".into(),
            tailscale_ips: vec![],
            online: true,
        };
        assert_eq!(n.endpoint(), None);
    }

    #[test]
    fn parse_uses_empty_dns_when_field_absent() {
        // A peer with no DNSName field at all is still parsed (was previously dropped by `?`).
        let json = r#"{ "Peer": {
            "k": { "HostName": "noproxy", "Online": true, "TailscaleIPs": ["100.5.5.5"] }
        }}"#;
        let nodes = parse_tailscale_status(json);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].dns_name, "");
        assert_eq!(nodes[0].endpoint().as_deref(), Some("100.5.5.5"));
    }

    #[test]
    fn probe_all_fans_out_to_every_node_using_endpoint() {
        let online = vec![
            Node {
                name: "a".into(),
                dns_name: "a.ts.net".into(),
                tailscale_ips: vec!["100.0.0.1".into()],
                online: true,
            },
            Node {
                name: "b".into(),
                dns_name: "".into(),
                tailscale_ips: vec!["100.0.0.2".into()],
                online: true,
            },
        ];
        // fake probe echoes the url back so we can assert which endpoint each node was addressed at.
        let seen = probe_all(&online, 7878, |url| vec![url.to_string()]);
        let urls: Vec<String> = seen.iter().map(|(u, _)| u.clone()).collect();
        assert_eq!(seen.len(), 2); // every online node probed
        assert!(urls.contains(&"http://a.ts.net:7878".to_string())); // dns_name used
        assert!(urls.contains(&"http://100.0.0.2:7878".to_string())); // IP fallback used
    }

    #[test]
    fn probe_all_brackets_ipv6_endpoint_in_url() {
        // An IPv6-only peer (no IPv4, no MagicDNS) must yield a bracketed authority `http://[v6]:port`,
        // else the address colons corrupt the host:port split.
        let online = vec![Node {
            name: "v6".into(),
            dns_name: "".into(),
            tailscale_ips: vec!["fd7a:1::1".into()],
            online: true,
        }];
        let seen = probe_all(&online, 7878, |url| vec![url.to_string()]);
        let urls: Vec<String> = seen.iter().map(|(u, _)| u.clone()).collect();
        assert!(
            urls.contains(&"http://[fd7a:1::1]:7878".to_string()),
            "IPv6 endpoint must be bracketed, got {urls:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_bounded_returns_fast_command_output() {
        let mut c = Command::new("sh");
        c.args(["-c", "printf hello"]);
        assert_eq!(
            capture_bounded(c, Duration::from_secs(5)).as_deref(),
            Some("hello")
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_bounded_times_out_on_a_wedged_command() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 5; printf late"]);
        assert_eq!(capture_bounded(c, Duration::from_millis(200)), None);
    }

    #[cfg(unix)]
    #[test]
    fn capture_bounded_returns_none_on_nonzero_exit() {
        // Restore the old exit-status gate: a non-zero `tailscale status` must NOT feed its stdout
        // into discovery (degrade-not-crash), even if it printed JSON-ish bytes.
        let mut c = Command::new("sh");
        c.args(["-c", "printf partial; exit 3"]);
        assert_eq!(capture_bounded(c, Duration::from_secs(5)), None);
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

    #[test]
    fn probe_agents_gives_up_fast_on_an_unreachable_host() {
        // 192.0.2.1 is RFC5737 TEST-NET-1 (unrouted) → the SYN is dropped, so the connect must be
        // bounded and give up in ~1s. The OS default connect timeout (~21s on Windows) made
        // `ensemble nodes` take ~30s on a tailnet that has idle iOS/Android peers dropping :7878.
        let start = Instant::now();
        let agents = probe_agents("http://192.0.2.1:7878");
        assert!(agents.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "probe must bound its connect; took {:?}",
            start.elapsed()
        );
    }
}
