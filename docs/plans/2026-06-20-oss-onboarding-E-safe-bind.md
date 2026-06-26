# Tick E — serve binds tailnet-only by default — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ensemble serve` defaults to binding the node's TailscaleIPv4 (reachable only over the tailnet), falls back to loopback (never `0.0.0.0`) when there is no tailnet IP, and keeps the explicit `--bind` override.

**Architecture:** Two pure functions + thin wiring. `discovery::self_tailscale_ips()` reads the local node's `Self.TailscaleIPs` from the already-bounded `tailscale status --json`. `serve::resolve_bind()` is a pure decision (explicit > tailnet IPv4 > loopback). `serve_cmd` in `main.rs` wires them and warns on the loopback fallback. No change to the `serve()` HTTP loop itself.

**Tech Stack:** Rust, serde_json, the repo's existing `discovery::capture_bounded`. Build/test via WSL (native debug hits Defender LNK1104): `wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test …'`.

**Spec:** `docs/specs/2026-06-20-oss-onboarding-design.md` §3 component E.

---

## File Structure

- `src/discovery.rs` — add `parse_self_ips` (pure) + `self_tailscale_ips` (IO). Reuses `capture_bounded`/`STATUS_TIMEOUT`.
- `src/serve.rs` — add `BindAddr` enum + `resolve_bind` (pure).
- `src/main.rs` — rewrite `serve_cmd` to use them; update the `USAGE` line.
- `src/lib.rs` — export the new `serve::{BindAddr, resolve_bind}` and `discovery::self_tailscale_ips`.

## Pre-step: branch

- [ ] **Create the feature branch (off current main).**

Run:
```bash
cd /d/Projects/ensemble && git checkout main && git pull --ff-only && git checkout -b feat/serve-tailnet-bind
```

---

## Task 1: `discovery::self_tailscale_ips` — read this node's tailnet IPs

**Files:**
- Modify: `src/discovery.rs` (add `parse_self_ips` + `self_tailscale_ips`; add tests in the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** (add inside `mod tests`)

```rust
    #[test]
    fn parse_self_ips_reads_self_tailscale_ips() {
        let json = r#"{ "Self": { "HostName": "node-a",
            "TailscaleIPs": ["100.x.y.z", "fd7a:1::5"] }, "Peer": {} }"#;
        assert_eq!(parse_self_ips(json), vec!["100.x.y.z", "fd7a:1::5"]);
    }

    #[test]
    fn parse_self_ips_empty_when_logged_out() {
        // logged-out status has Self.TailscaleIPs = null
        let json = r#"{ "Self": { "HostName": "node-a", "TailscaleIPs": null }, "Peer": {} }"#;
        assert!(parse_self_ips(json).is_empty());
        assert!(parse_self_ips("not json").is_empty());
    }
```

- [ ] **Step 2: Run the tests, verify they fail**

Run:
```bash
wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib parse_self_ips 2>&1 | tail -15'
```
Expected: FAIL to compile — `cannot find function parse_self_ips`.

- [ ] **Step 3: Implement** (add above `#[cfg(test)]` in `src/discovery.rs`)

```rust
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
```

- [ ] **Step 4: Run the tests, verify they pass**

Run:
```bash
wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib parse_self_ips 2>&1 | tail -8'
```
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/discovery.rs && git commit -m "feat(discovery): self_tailscale_ips — read this node's tailnet IPs"
```

---

## Task 2: `serve::resolve_bind` — the bind decision

**Files:**
- Modify: `src/serve.rs` (add `BindAddr` + `resolve_bind` + tests)

- [ ] **Step 1: Write the failing test** (add a `#[cfg(test)] mod tests` block at the end of `src/serve.rs`, or extend it if present)

```rust
#[cfg(test)]
mod bind_tests {
    use super::*;

    #[test]
    fn explicit_override_wins() {
        let b = resolve_bind(&["100.1.2.3".into()], Some("0.0.0.0:9999"), 7878);
        assert_eq!(b, BindAddr::Explicit("0.0.0.0:9999".into()));
    }

    #[test]
    fn prefers_tailnet_ipv4() {
        let ips = vec!["fd7a:1::5".to_string(), "100.x.y.z".to_string()];
        let b = resolve_bind(&ips, None, 7878);
        assert_eq!(b, BindAddr::Tailnet("100.x.y.z:7878".into()));
    }

    #[test]
    fn loopback_when_no_tailnet_ip() {
        let b = resolve_bind(&[], None, 7878);
        assert_eq!(b, BindAddr::Loopback("127.0.0.1:7878".into()));
    }

    #[test]
    fn addr_accessor_returns_the_string() {
        assert_eq!(resolve_bind(&[], None, 7878).addr(), "127.0.0.1:7878");
    }
}
```

- [ ] **Step 2: Run the tests, verify they fail**

Run:
```bash
wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib bind_tests 2>&1 | tail -15'
```
Expected: FAIL to compile — `cannot find type BindAddr` / `function resolve_bind`.

- [ ] **Step 3: Implement** (add near the top of `src/serve.rs`, after the imports)

```rust
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
```

- [ ] **Step 4: Run the tests, verify they pass**

Run:
```bash
wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib bind_tests 2>&1 | tail -8'
```
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
git add src/serve.rs && git commit -m "feat(serve): resolve_bind — tailnet-IP default, loopback fallback"
```

---

## Task 3: wire `serve_cmd` + exports + usage

**Files:**
- Modify: `src/lib.rs` (exports)
- Modify: `src/main.rs:57-64` (`serve_cmd`) and `src/main.rs:24` (USAGE line)

- [ ] **Step 1: Export the new API** — in `src/lib.rs`, change the serve + discovery `pub use` lines so they include the new items. Find:

```rust
pub use serve::serve;
```
and replace with:
```rust
pub use serve::{resolve_bind, serve, BindAddr};
```
Then find the discovery `pub use` block and add `self_tailscale_ips` to it (it already exports `discover_agent_hosts, discover_nodes, …`).

- [ ] **Step 2: Rewrite `serve_cmd`** in `src/main.rs` (replace lines 57-64):

```rust
fn serve_cmd(args: &[String]) {
    let explicit = parse_flag(args, "--bind");
    // Default to the tailnet interface so serve is reachable only over the tailnet, not the LAN.
    let self_ips = ensemble::discovery::self_tailscale_ips();
    let bind = ensemble::resolve_bind(&self_ips, explicit.as_deref(), 7878);
    if let ensemble::BindAddr::Loopback(_) = bind {
        eprintln!(
            "ensemble: no tailnet IP found (is tailscale up?) — binding loopback only (local). \
             Pass --bind <addr> to override."
        );
    }
    let addr = bind.addr().to_string();
    println!("ensemble serve on {addr}");
    if let Err(e) = ensemble::serve(&addr, adapters()) {
        eprintln!("serve: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 3: Update the USAGE line** — in `src/main.rs:24`, replace:

```rust
    ensemble serve [--bind <addr>]   (default 0.0.0.0:7878 — host this node's local agents)\n\n\
```
with:
```rust
    ensemble serve [--bind <addr>]   (default: this node's tailnet IP:7878; loopback if no tailnet)\n\n\
```

- [ ] **Step 4: Build + full test + clippy, verify green**

Run:
```bash
wsl bash -lc 'cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test 2>&1 | grep -E "test result|error|warning:" ; CARGO_TARGET_DIR=$HOME/ensemble-target cargo clippy --all-targets >/tmp/c.txt 2>&1; echo "clippy=$?"; grep -cE "^warning|^error" /tmp/c.txt'
```
Expected: all `test result: ok`, `clippy=0`, `0` warn/err.

- [ ] **Step 5: Empirical check (real machine, optional but recommended)** — rebuild native to a clean dir and confirm the default bind is the tailnet IP:

```bash
CARGO_TARGET_DIR=C:/Users/<you>/ensemble-target-rel cargo build --release 2>&1 | tail -2
# Then: start serve and confirm it prints "ensemble serve on 100.x:7878" (Ctrl-C to stop)
```

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/main.rs && git commit -m "feat(serve): serve_cmd binds tailnet IP by default (loopback fallback)"
```

---

## Task 4: double-gate + land

- [ ] **Step 1: Build the gate prompt** (diff vs main) into a repo-OUTSIDE temp file:

```bash
P="/c/Users/<you>/AppData/Local/Temp/ens_gate_E.txt"
printf 'You are a Rust reviewer. Review ONLY the diff; reply with findings then a final line `VERDICT: LGTM` or `VERDICT: CHANGES`. Do NOT edit files or run git.\nCONTEXT: ensemble serve now defaults to binding the tailnet IPv4 (reachable only over the tailnet) instead of 0.0.0.0; loopback fallback when no tailnet IP (never 0.0.0.0); --bind still overrides. parse_self_ips reads Self.TailscaleIPs; resolve_bind is the pure decision.\nSCRUTINIZE: (a) resolve_bind precedence + the never-0.0.0.0 invariant; (b) parse_self_ips on logged-out/null; (c) does binding 100.x fail if tailscaled is up but the IP is not yet assigned? behavior then; (d) any regression for users who relied on the old 0.0.0.0 default (they must use --bind 0.0.0.0:7878 now — is that documented?).\n=== DIFF ===\n' > "$P"
git --no-pager diff main >> "$P"
```

- [ ] **Step 2: Run codex + claude gates SAFELY** (codex from an EMPTY temp cwd so it cannot mutate the repo — see the gate-mutation lesson in the backlog). Both in the background:

```
# codex (background): cd /c/Users/<you>/AppData/Local/Temp/ens_gate_cwd && cmd //C codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check < /c/Users/<you>/AppData/Local/Temp/ens_gate_E.txt
# claude (background): cd /c/Users/<you>/AppData/Local/Temp/ens_gate_cwd && cmd //C claude -p < /c/Users/<you>/AppData/Local/Temp/ens_gate_E.txt
```
Expected: ≥2 distinct-AI `VERDICT: LGTM`. If CHANGES, fix + re-gate. After the gate, run `git status` to confirm the tree is untouched.

- [ ] **Step 3: Land** — merge ff-only + push:

```bash
git checkout main && git merge --ff-only feat/serve-tailnet-bind && git push origin main && git branch -d feat/serve-tailnet-bind
```

- [ ] **Step 4: Refresh the deployed binary** — rebuild native to the clean out-of-repo dir (`CARGO_TARGET_DIR=C:/Users/<you>/ensemble-target-rel cargo build --release`) and re-Taildrop to active peers; then update the backlog Log + commit + push.

---

## Self-Review (done at write time)

- **Spec coverage:** §3-E (tailnet-IP default, loopback fallback never 0.0.0.0, `--bind` override) — Tasks 1-3. ✓
- **Placeholders:** none — every step has concrete code/commands.
- **Type consistency:** `parse_self_ips`/`self_tailscale_ips` (Task 1) used by `serve_cmd` (Task 3); `BindAddr`/`resolve_bind` (Task 2) exported in Task 3 and matched in `serve_cmd`. ✓
- **Out of scope (later ticks):** the `[discovery] port` knob, `ensemble up`/`mesh`, service install, distribution.
