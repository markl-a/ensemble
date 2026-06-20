//! `ensemble mesh` — the status view: which AI CLIs are on THIS node + which agents each discovered
//! tailnet peer hosts. The render is pure (`render_mesh`); the `mesh` command gathers the data from
//! `doctor` (local CLIs on PATH) and `discovery` (tailnet host → agents).

/// Render the mesh view: the present local CLIs, then each reachable tailnet host and the agents it
/// hosts. `hosts` is `(endpoint_url, agents)` per reachable peer. Pure.
pub fn render_mesh(local_clis: &[String], hosts: &[(String, Vec<String>)]) -> String {
    let local = if local_clis.is_empty() {
        "(none on PATH)".to_string()
    } else {
        local_clis.join(", ")
    };
    let mut s = format!("local CLIs : {local}\n");
    if hosts.is_empty() {
        s.push_str("tailnet    : (none discovered — are peers running `ensemble serve`?)");
    } else {
        s.push_str("tailnet    :");
        for (i, (host, agents)) in hosts.iter().enumerate() {
            let prefix = if i == 0 { " " } else { "\n             " };
            s.push_str(&format!("{prefix}{host} → {}", agents.join(", ")));
        }
    }
    s
}

/// Render the `ensemble up` startup banner: where it's serving, then the indented mesh view, then a
/// "serving… Ctrl-C to stop" footer. Pure (the command prints this, then blocks on `serve`).
pub fn render_up(addr: &str, local_clis: &[String], hosts: &[(String, Vec<String>)]) -> String {
    let mesh = render_mesh(local_clis, hosts);
    let indented = mesh
        .lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("ensemble up — serving on {addr}\n{indented}\n(serving… Ctrl-C to stop)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_up_shows_banner_indented_mesh_and_footer() {
        let out = render_up("100.87.70.65:7878", &["codex".to_string()], &[]);
        assert!(
            out.contains("ensemble up — serving on 100.87.70.65:7878"),
            "got:\n{out}"
        );
        assert!(out.contains("\n  local CLIs : codex"), "indented; got:\n{out}");
        assert!(out.contains("(serving"), "footer; got:\n{out}");
    }

    #[test]
    fn renders_local_and_tailnet() {
        let local = vec!["codex".to_string(), "claude".to_string()];
        let hosts = vec![
            (
                "http://ayaneo:7878".to_string(),
                vec!["codex".to_string(), "claude".to_string()],
            ),
            (
                "http://dev-host:7878".to_string(),
                vec!["agy".to_string(), "opencode".to_string()],
            ),
        ];
        let out = render_mesh(&local, &hosts);
        assert!(out.contains("local CLIs : codex, claude"), "got:\n{out}");
        assert!(
            out.contains("http://ayaneo:7878 → codex, claude"),
            "got:\n{out}"
        );
        assert!(
            out.contains("http://dev-host:7878 → agy, opencode"),
            "got:\n{out}"
        );
    }

    #[test]
    fn renders_empty_state() {
        let out = render_mesh(&[], &[]);
        assert!(out.contains("local CLIs : (none on PATH)"), "got:\n{out}");
        assert!(out.contains("tailnet    : (none discovered"), "got:\n{out}");
    }
}
