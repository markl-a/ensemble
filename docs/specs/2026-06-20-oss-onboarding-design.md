# ensemble — OSS onboarding ("install → just works") design

> Status: DESIGN (brainstorm-approved 2026-06-20). Turns the manual fleet-bootstrap
> (Taildrop a binary, hand-run `serve`, open firewalls, fix SSH) into the intended
> open-source experience: install the package, and ensemble auto-recognizes the local
> AI CLIs and the AI CLIs on tailnet machines and orchestrates them together.

## 1. Purpose

The orchestration engine already exists: `ensemble doctor` detects local vendor CLIs on
PATH, tailnet auto-discovery finds peers running `ensemble serve` and which agents they
host, and the conductor/crew drives them together. What is missing is the **on-ramp** that
makes this real for an open-source user who just installed the tool. This spec defines that
on-ramp.

**Target experience (end state):**
```
1. cargo install ensemble            # or download a release binary
2. (you are already logged into your CLIs: claude / codex / agy / opencode)
3. ensemble up                       # starts serve + prints the mesh
   → local CLIs : claude, codex, agy, opencode
   → tailnet    : ayaneo → [codex, claude]   dev-host → [opencode, agy]
4. use it: ensemble agent codex "…"   |   ensemble run "…"
```

## 2. Trust model (decided)

**Network trust = the tailnet is the trust boundary.** Anyone who can reach a node over the
tailnet may drive its CLIs; there is no app-level token, identity check, or TLS. This is the
zero-config default that matches the "just works" goal. The docs MUST state the boundary
plainly: the tailnet is the security perimeter; a shared/multi-user tailnet should restrict
who can reach port 7878 with **tailscale ACLs**. Component E narrows the exposure so this
boundary actually holds (serve is reachable only over the tailnet, not the LAN/public).

**Non-goals (YAGNI):** app-level auth, shared tokens, TLS certificates, per-call identity
verification, and non-tailscale discovery (mDNS/broadcast). The tailnet already provides the
network, addressing, and (if the user wants it) ACLs.

## 3. Components

Each component is an **independent tick** — its own plan, TDD implementation, and double-gate.
They are labeled A–E by area (not build order — §4 gives the sequence). "Depends on" lists only
already-landed pieces.

### E. Safe bind default — serve is reachable only over the tailnet
- **Today:** `ensemble serve` binds `0.0.0.0:7878` — every interface, including the LAN and any
  public NIC. On the design's "serve runs by default on every machine" model that is too broad.
- **Change:** default-bind to the node's **TailscaleIPv4** (`100.x`), so serve is reachable over
  the tailnet only. Resolve it from `tailscale status --json` `Self.TailscaleIPs` (reusing the
  bounded `capture_bounded` path) or `tailscale ip -4`.
- **Fallbacks (never silently widen):** if no tailnet IP is available (tailscale absent/logged
  out), bind **loopback** (`127.0.0.1`) and print a clear one-line warning — never fall back to
  `0.0.0.0`. The explicit `--bind <addr>` override stays (e.g. `--bind 0.0.0.0:7878` for a
  non-tailscale LAN, `--bind 127.0.0.1:7878` for local-only).
- **Depends on:** serve, discovery (`capture_bounded`).
- **Testing:** pure `resolve_bind_addr(self_ips, explicit_override, port) -> SocketAddr` —
  tailnet-IP present → that; none + no override → loopback; override → honored. Hermetic.

### B. `ensemble up` — one command to start serving and see the mesh
- **Behavior:** resolve the bind address (E) → print the readiness + mesh view (D) → start
  `serve` in the **foreground** and keep serving until Ctrl-C. Foreground keeps `up` simple and
  obviously-running; the background/boot story is component C, not `up` (so there is no
  `ensemble down` to reason about).
- **Output (before it blocks on serve):**
  ```
  ensemble up — serving on 100.87.70.65:7878
    local CLIs : codex, claude, agy, opencode
    tailnet    : ayaneo → [codex, claude]   dev-host → [opencode, agy]
  (serving… Ctrl-C to stop)
  ```
- **Depends on:** doctor, discovery, serve, component E.
- **Testing:** the render is the pure D function; the serve loop is the existing tested path.
  `up` itself is thin glue (smoke-level).

### D. `ensemble mesh` — the status view ("what can I see?")
- **Behavior:** read-only. Print local CLIs (doctor) + each discovered tailnet host → the agents
  it hosts (discovery). No side effects. This is the "recognize local + tailnet CLIs"
  visibility, usable on its own and reused by `up`.
- **Depends on:** doctor, discovery.
- **Testing:** pure `render_mesh(local: &[ToolStatus], hosts: &HashMap<String,Vec<String>>)
  -> String` — hermetic, covering empty-tailnet and multi-host cases.

### C. `serve --install-service` / `--uninstall-service` — persistent, boot-started serve
- **Behavior:** register an OS unit that runs `ensemble serve` at login/boot, and remove it.
  Use the lightweight per-OS mechanism (no Windows SCM service-control plumbing needed):
  - **Windows:** a Task Scheduler logon task (`schtasks /create … /sc onlogon`) running
    `ensemble serve`.
  - **macOS:** a launchd **user agent** plist in `~/Library/LaunchAgents/com.ensemble.serve.plist`,
    `launchctl load`.
  - **Linux:** a **systemd user** unit `~/.config/systemd/user/ensemble.service` + `systemctl
    --user enable --now`.
- **Heaviest / most platform-specific → built last but one.** Each unit file/command is generated
  from (binary path, bind args) — pure to generate, then a thin IO step that writes + enables.
- **Depends on:** serve, component E (the service should inherit the safe bind default).
- **Testing:** pure generators (`launchd_plist`, `systemd_unit`, `schtasks_argv`) unit-tested on
  content given paths; install/uninstall is `#[ignore]` per-OS smoke.

### A. Distribution — how users get the binary
- `cargo install ensemble` (publish to crates.io).
- **GitHub Releases** prebuilt binaries via CI (GitHub Actions on tag) for: Windows
  `x86_64-pc-windows-msvc`, macOS `aarch64-apple-darwin` + `x86_64-apple-darwin`, Linux
  `x86_64-unknown-linux-gnu`. Naming follows the `cargo-binstall` convention so `cargo binstall
  ensemble` works for free.
- README "Install" section documents all three paths.
- **Built last** (release engineering; nothing depends on it). Mostly CI + a crates.io publish.
- **Testing:** CI build matrix is the test; a tagged dry-run validates the artifacts.

## 4. Build sequence

`E → D → B → C → A`, each its own double-gated tick (D before B — `up` reuses D's render):
1. **E** safe bind — smallest, reduces real exposure, unblocks the "serve everywhere" model.
2. **D** `ensemble mesh` — the pure mesh render, usable on its own and reused by `up`.
3. **B** `ensemble up` — makes the experience tangible (start serve + show the mesh via D).
4. **C** service install — heavier, per-OS; the "permanent" half of the serve-persistence decision.
5. **A** distribution — release engineering, done when the rest is ready to ship.

## 5. What already exists (not redesigned)

- `ensemble doctor` — local CLI + tailscale + git-repo detection (`check_tools`/`run_checks`).
- Auto-discovery — `discover_agent_hosts` (bounded `tailscale status`, parallel `/health`,
  MagicDNS-off TailscaleIP fallback).
- `ensemble serve` — the agent-host (binds a port, serves `/run` + `/health`).
- conductor / crew / adapters — the orchestration that drives local + remote CLIs together.

The vendor CLIs manage their own auth (you log into claude/codex/agy/opencode separately);
ensemble only drives whatever is on PATH, so "login → it recognizes them" needs no new work
beyond making it visible (D).
