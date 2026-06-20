# ensemble — two real-test phases → open-source (design)

> Status: DESIGN (brainstorm + web-research-grounded, approved 2026-06-20; Phase 1 shape approved).
> Driven by a parallel research workflow (6 OSS dives + an ensemble current-state map). Verdict:
> ensemble already HAS ~70% of Phase 1 / ~60% of Phase 2 as a library; NO single OSS covers its
> 4-axis intersection (governance + cross-vendor + cross-machine-over-Tailscale + inter-agent comms),
> so the conductor stays BUILD and we ADOPT narrowly. The endpoints are blocked by a small set of
> missing glue, not architecture.

## Goal — two operator tests, each ending in real use

- **PHASE 1 (single machine):** in one repo, a typed prompt runs the 4-vendor crew
  (codex + claude + opencode + agy); the agents collaborate via the blackboard; each role's CLI works
  in its own git worktree; the test gate + quorum pass; the landed branch is merged to main. The
  prompt-receiving CLI is **mesh-aware** — it understands which machines/CLIs are connected and how to
  hand work out. **Endpoint:** the operator types one prompt and watches the 4 REAL CLIs do this on
  their machine.
- **PHASE 2 (cross machine):** several machines on the same Tailscale account each run ensemble as a
  boot-started service that auto-grabs its own tailnet IP and auto-connects to peers; a crew dispatched
  from one machine drives CLIs on another, with the agent-host authenticating callers. **Endpoint:**
  the operator reinstalls clean, connects, and applies the cross-machine crew to real **phantom-mesh**
  development — zero manual networking.
- **Then:** clean up and **open-source** (trivial public install via `dist` + cargo-binstall).

## Locked decisions (operator, 2026-06-20)

1. **Workers can be LIVE interactive CLIs OR ensemble-spawned — BOTH, through one substrate**
   (operator: "最起碼要是活 CLI session，然後 ensemble 也能自己拉起 CLI"). The substrate is the
   blackboard (shared state) + worktree-per-task + the `ensemble merge` verb, exposed to live CLIs via
   an **ensemble MCP server** (`ensemble mcp`), NOT a bespoke REPL — the CLIs you already open ARE the
   interface. A live CLI (claude / codex / opencode as an MCP client) becomes a first-class CREW MEMBER:
   it reads the mesh + blackboard, claims/assigns work, gets its OWN worktree, posts results, and merges
   — via MCP tools (`ensemble_mesh`, `ensemble_board_read`/`ensemble_board_post`, `ensemble_claim`,
   `ensemble_worktree`, `ensemble_merge`, `ensemble_run`). ensemble ALSO drives headless CLI turns (the
   adapters) as spawned workers over the SAME primitives. **Flow:** the operator prompts any live lead
   CLI → it decomposes the task + posts assignments to the board → live members AND/OR spawned workers
   each claim a part, work in their OWN worktree, coordinate via the board → results merge. A CLI with no
   MCP-client mode or that flakes (agy on Windows) stays reachable as a spawned worker (the adapter path).
2. **`ensemble merge` conflict policy = spawn ONE AI-resolver round** (a CLI attempts the resolution);
   only if it still conflicts, escalate cleanly to the operator. (Never silently force/auto-accept.)
3. **`ensemble run "<prompt>"` is the Phase-1 task entry** (the prompt-as-arg IS the operator's prompt);
   no persistent REPL — an interactive lead CLI shells to ensemble verbs / the MCP server.
4. **serve caller-auth approach = TBD** (decided during Phase-2 analysis: `tailscale whois` shell-out vs
   the tailscale-localapi crate; plain-HTTP-over-WireGuard vs `tailscale serve` TLS).

## Adopt vs build (from the research)

| Capability | Decision | Source |
|---|---|---|
| Conductor pipeline + quorum/test gate | **KEEP (build)** | ensemble — no OSS covers the intersection |
| Mediated blackboard (summary→prompt injection) | **KEEP**, improve to bounded/role-routed | ensemble; validated vs bernstein / agent-hub-mcp / ACP |
| `ensemble merge` verb | **BUILD** on worktree.rs/repo_sync.rs | conservative policy from orc / container-use |
| Mesh-aware entry | **BUILD** `ensemble mcp` (thin) | official Rust `rmcp` SDK |
| serve caller-auth | **BUILD** | wolfpack + `tailscale whois` (or tailscale-localapi crate) |
| Boot service install | **ADOPT** | `service-manager` + `windows-service` crates (= backlog C) |
| Distribution | **ADOPT** | `dist` (axodotdev) + cargo-binstall (= backlog A) |
| Cross-machine shared ledger | **BUILD** (extend ledger.rs) | = backlog 3b-2b; patterns from AgentsMesh (study-only) |
| ACP-client adapter | **HYBRID, defer** | agent-client-protocol SDK; subprocess stays default + only path for agy |
| Signed proofpack | **DEFER to pre-open-source** | bernstein (Apache-2.0) HMAC-chained audit log |

## Phase 1 — steps (each ends runnable + verifiable)

0. **Baseline:** `cargo test` green (pipeline_hermetic / test_gate_e2e / firewall_b) — the floor every step keeps.
1. **`ensemble merge <branch> [--into main]`** — land a kept `ensemble/<slug>` branch: ff where possible
   else true-merge; on conflict run ONE AI-resolver round, else escalate with the conflicting paths.
   Wire `Decision::Landed` so `ensemble run --merge` can auto-land. *(Phase-1 #1 blocker.)*
2. **Per-run journal** — append the blackboard transcript + each round's test result + the final decision
   to `.ensemble/runs/<slug>.jsonl` (reuse `Message`). The operator can SEE the collaboration.
3. **`ensemble mcp` server (the crew-participation API)** — expose the substrate over MCP (stdio,
   `rmcp`): `ensemble_mesh`, `ensemble_board_read`/`ensemble_board_post`, `ensemble_claim`,
   `ensemble_worktree`, `ensemble_merge`, `ensemble_run`. This makes a LIVE CLI a first-class crew member
   (decision 1): the operator prompts it, it decomposes + posts assignments, and live members and/or
   ensemble-spawned workers each claim a part in their own worktree. Build incrementally: read-only
   `mesh`/`board_read` first, then `board_post`/`claim`/`worktree`/`merge`.
4. **Live 4-CLI proof** — crew.toml: implement=codex, review=claude, debug=agy, `[test]`=the repo's real
   test; run a small real task in a throwaway repo (after `ensemble doctor` green). The live equivalent of
   pipeline_hermetic.rs with real adapters. Land + merge; the journal shows all 4 participated.
5. **Harden the flake points the live run exposes** (only if surfaced): verdict parse robustness; a
   per-command timeout in test_gate.rs (firewall-A slice-1).

**Phase-1 endpoint:** operator types ONE prompt → codex implements in its worktree, claude + agy
review/debug via the blackboard, firewall-A goes GREEN, quorum APPROVES, `ensemble merge` lands it — with
the 4 REAL vendor CLIs; the journal proves the collaboration.

## Phase 2 — steps (outline; details finalized when we get there)

1. **Secure the agent-host** — authenticate every POST `/run` by the caller's tailnet identity
   (approach TBD per decision 4) + an allow-policy. *(Phase-2 #1 hole: serve currently has zero auth.)*
2. **Boot service** (`ensemble service install|start|stop|uninstall|status`, user-level) — backlog C.
3. **Live cross-machine drive** — machine A's run discovers + drives machine B's CLI; git-bundle sync
   lands B's edits (the chain cross_machine.rs proves in-process, now on the real tailnet).
4. **Cross-machine shared ledger** — at-most-once claim ACROSS nodes + node registry/heartbeat/leases
   (backlog 3b-2b).
5. **Reinstall-and-apply dress rehearsal** — clean machine: install → `service install` → auto-connect →
   run a REAL phantom-mesh task across machines, governed + authenticated + merged. *(Operator endpoint.)*
6. **Deferred:** `ensemble agent` live token streaming (backlog 10); optional ACP adapter.

## Open-source (after Phase 2)

Distribution (`dist` + cargo-binstall + install.sh/ps1 — backlog A); README/CONTRIBUTING polish; the
signed/hash-chained proofpack (bernstein-style — backlog 4) as the headline governance differentiator;
scrub any private IPs/hostnames.

## Open questions deferred to their phase

serve-auth source + transport (Phase 2 step 1); cross-machine ledger transport — HTTP pull-fleet vs
shared-FS vs git-branch-queue (Phase 2 step 4); distribution targets + crates.io name + Homebrew/MSI
(open-source); proofpack-before-or-after-launch.
