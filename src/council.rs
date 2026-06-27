//! Council broadcast (`ensemble all`): fan ONE prompt to EVERY AI CLI ensemble can reach — each LOCAL
//! cli plus every agent on every discovered tailnet peer — run each as one read-only turn, and render
//! all replies side by side. No worktree, no gate, no land: pure compare-the-fleet-on-one-question
//! (backlog item 0.7). This module is the PURE core (target enumeration + label + render); the parallel
//! fan-out IO shell lives in main.rs.

use std::collections::HashSet;

/// One Council fan-out target.
#[derive(Debug, Clone, PartialEq)]
pub struct CouncilTarget {
    pub agent: String,
    /// `None` = run the agent LOCALLY; `Some(url)` = drive it on a tailnet peer's `ensemble serve`.
    pub node: Option<String>,
    /// Display label, e.g. `codex@local` or `codex@node-b`.
    pub label: String,
}

/// Enumerate every reachable agent: each LOCAL cli (`node = None`) + each agent on each discovered host
/// (`node = that host's serve URL`). `mesh` is `discovery::discover_mesh`'s `(serve_url, agents)` per
/// host. Deduped by label so the same `(agent, host)` is never queried twice.
pub fn council_targets(local: &[String], mesh: &[(String, Vec<String>)]) -> Vec<CouncilTarget> {
    let mut out: Vec<CouncilTarget> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cli in local {
        let label = format!("{cli}@local");
        if seen.insert(label.clone()) {
            out.push(CouncilTarget {
                agent: cli.clone(),
                node: None,
                label,
            });
        }
    }
    for (url, agents) in mesh {
        let host = short_host(url);
        for agent in agents {
            let label = format!("{agent}@{host}");
            if seen.insert(label.clone()) {
                out.push(CouncilTarget {
                    agent: agent.clone(),
                    node: Some(url.clone()),
                    label,
                });
            }
        }
    }
    out
}

/// A short, human label for a serve URL: strip the scheme and `:port`, then take the first DNS label
/// (`http://node-b.example.ts.net:7878` → `node-b`). An IPv4 or bracketed IPv6 literal is kept whole
/// (minus scheme/port) — it has no meaningful short label.
pub fn short_host(url: &str) -> String {
    let s = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    // [v6]:port → keep "[v6]"
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((v6, _)) = rest.split_once(']') {
            return format!("[{v6}]");
        }
    }
    // host:port → host (only strip a trailing all-digit :port, not a colon inside the host)
    let host = match s.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => s,
    };
    // an IPv4 (every dotted segment numeric) is kept whole; a DNS name → its first label
    let is_ipv4 = host.contains('.')
        && host
            .split('.')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()));
    if is_ipv4 {
        host.to_string()
    } else {
        host.split('.').next().unwrap_or(host).to_string()
    }
}

/// Render the Council results (`label → reply or error string`) as labeled blocks for the terminal,
/// with a trailing `replied N/M` tally. A flaked/unavailable agent is shown as `[flaked: …]`, never
/// hidden — the point of a council is seeing who said what (and who couldn't).
pub fn render_council(results: &[(String, Result<String, String>)]) -> String {
    let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
    let mut s = String::new();
    for (label, r) in results {
        match r {
            Ok(text) => s.push_str(&format!("\n=== {label} ===\n{}\n", text.trim())),
            Err(e) => s.push_str(&format!("\n=== {label} ===\n[flaked: {e}]\n")),
        }
    }
    s.push_str(&format!("\n— council: {ok}/{} replied —\n", results.len()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn council_targets_enumerates_local_and_every_remote_agent() {
        let local = vec!["codex".to_string(), "claude".to_string()];
        let mesh = vec![(
            "http://node-b.example.ts.net:7878".to_string(),
            vec!["codex".to_string(), "agy".to_string()],
        )];
        let t = council_targets(&local, &mesh);
        let labels: Vec<&str> = t.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"codex@local") && labels.contains(&"claude@local"));
        assert!(
            labels.contains(&"codex@node-b") && labels.contains(&"agy@node-b"),
            "got {labels:?}"
        );
        // a local cli has no node; a remote one carries the serve URL
        let local_codex = t.iter().find(|c| c.label == "codex@local").unwrap();
        assert_eq!(local_codex.node, None);
        let remote_codex = t.iter().find(|c| c.label == "codex@node-b").unwrap();
        assert_eq!(
            remote_codex.node.as_deref(),
            Some("http://node-b.example.ts.net:7878")
        );
        assert_eq!(remote_codex.agent, "codex");
    }

    #[test]
    fn council_targets_dedupe_by_label() {
        // same host appearing twice (or a local cli listed twice) yields one target per (agent,host)
        let local = vec!["codex".to_string(), "codex".to_string()];
        let mesh = vec![
            (
                "http://h.ts.net:7878".to_string(),
                vec!["claude".to_string()],
            ),
            (
                "http://h.ts.net:7878".to_string(),
                vec!["claude".to_string()],
            ),
        ];
        let t = council_targets(&local, &mesh);
        assert_eq!(t.iter().filter(|c| c.label == "codex@local").count(), 1);
        assert_eq!(t.iter().filter(|c| c.label == "claude@h").count(), 1);
    }

    #[test]
    fn short_host_strips_scheme_port_and_domain() {
        assert_eq!(short_host("http://node-b.example.ts.net:7878"), "node-b");
        assert_eq!(short_host("http://100.64.12.34:7878"), "100.64.12.34"); // IPv4 kept whole
        assert_eq!(short_host("http://[fd7a::1]:7878"), "[fd7a::1]"); // v6 kept whole
        assert_eq!(short_host("http://node-a:7878"), "node-a"); // bare host, no domain
    }

    #[test]
    fn render_council_shows_every_reply_and_a_tally() {
        let results = vec![
            (
                "codex@local".to_string(),
                Ok("risk A: unbounded loop".to_string()),
            ),
            (
                "agy@node-b".to_string(),
                Err("agy flaked: timed out".to_string()),
            ),
        ];
        let out = render_council(&results);
        assert!(out.contains("codex@local") && out.contains("risk A"));
        assert!(out.contains("agy@node-b") && out.contains("[flaked: agy flaked: timed out]"));
        assert!(out.contains("1/2 replied"), "tally: {out}");
    }
}
