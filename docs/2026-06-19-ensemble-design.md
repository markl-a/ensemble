# ensemble — design

> Status: DESIGN (brainstorm-approved 2026-06-19). Reference findings (§4) land from two
> running studies: a single-machine multi-CLI deep-read (8 repos) + a cross-machine/
> Tailscale deep-read (5 repos). Implementation starts after §4 is filled.

## 1. Purpose (one paragraph)

**ensemble is a local-first, governed orchestrator that turns several different-vendor AI
coding CLIs — Claude Code, OpenAI Codex, Google Antigravity (`agy`), and opencode — into a
single AI development crew that can collaboratively develop, review, and debug one project.**
The crew works two ways at once: **parallel tasks** (many agents, each on its own task in an
isolated git worktree) and a **role pipeline** (one task flows implement → review → debug
across different vendors). The agents **communicate with each other** through a mediated
blackboard. The subscriptions/CLIs may live on **several machines connected by Tailscale**,
and ensemble coordinates them across the tailnet. What makes it different from the existing
multi-CLI runners (vibe-kanban, bernstein, emdash, councils): the **intersection** of
*governance* (quorum gate, graceful degradation when a vendor flakes, signed provenance of
who-did-what) + *cross-vendor* + *cross-machine over Tailscale* + *inter-agent communication*
— which the OSS landscape does not yet cover as one tool.

## 2. Why (the gap)

Verified research (2026-06): the high-star multi-CLI tools are either single-machine
(vibe-kanban, emdash, bernstein), API-council not cross-vendor-CLI (llm-council, pal/zen), or
multi-host-without-governance (OpenHands, ruflo, helix). The governed + cross-vendor +
cross-machine + inter-agent-comms intersection is unclaimed. ensemble builds on prior work
driving multiple vendor CLIs headlessly and coordinating tailnet meshes, generalizing the
smallest defensible novel slice as a standalone Apache-2.0 tool.

## 3. Architecture

```
                    ensemble (CLI + optional daemon)
                                |
   ┌────────────┬───────────────┼────────────────┬─────────────┐
   │ conductor  │  blackboard   │   gate         │  adapters    │  transport
   │ (schedule  │  (inter-agent │  (quorum +     │  (per-vendor │  (local | tailnet)
   │  甲+乙)    │   comms bus)  │   degrade)     │  headless)   │
   └────────────┴───────────────┴────────────────┴─────────────┘
        │              │               │                │              │
   worktree per   append-only      ≥k APPROVE,     claude/codex/    orchestrator ↔
   task/agent     shared msgs,     bounded rounds, opencode/agy     worker nodes
                  conductor routes flake-excluded   (hardened        over Tailscale
                  to next agent    don't block      headless)        (remote adapter)
```

- **adapters/** — one per vendor. `run(agent, prompt, cwd) -> AgentOutput`. Encodes the
  headless invocation contract (stdin-close, agy ConPTY/PTY, ANSI strip, Windows `cmd /C`
  shims). A flake/empty/rate-limit returns a *typed* error so the gate can degrade instead of
  faking a pass. Generalized from a prior hardened set of headless per-vendor CLI adapters.
  **Adapter details (esp. agy) pinned by §4 single-machine study.**
- **blackboard/** — per task-run append-only message channel. `post(from, kind, body)` /
  `read(since)`. THE inter-agent-comms mechanism: agents can't talk directly (they're
  subprocesses), so each posts messages (verdicts, questions, findings) and the **conductor
  delivers the relevant ones into the next agent's prompt**. Append-only now; hash-chained
  signing deferred to the governance phase. **Mechanism confirmed against real repos in §4.**
- **conductor/** — reads a `crew.toml` (roles → vendors + the pipeline), opens a git worktree
  per task, runs the role pipeline (乙) routing blackboard messages between steps, loops on
  CHANGES (bounded), and runs N pipelines in parallel (甲). Degrades when an agent flakes.
- **gate/** — computes a quorum decision (≥k APPROVE, bounded convergence rounds; a
  flake-excluded agent doesn't block) → land (merge worktree) / iterate / escalate.
- **transport/** — local (subprocess) for same-machine agents; **tailnet (remote)** for
  agents on other machines. **Phase 3.** Pinned by §4 cross-machine study.

### Data flow — role pipeline (乙) on one task
task → conductor opens worktree → `codex` implements + posts "did X" to blackboard →
`claude` reads diff + blackboard → posts verdict (APPROVE / CHANGES + message) → if CHANGES:
`codex` receives claude's message (inter-agent comm) + revises → bounded loop → `agy` runs
tests / finds bugs + posts findings → gate computes quorum → merge worktree or escalate.

### Error handling
Any agent flake/empty/rate-limit → typed degrade: retry once → substitute a backup vendor →
or exclude from quorum with a logged reason. **Never fake a pass.** (Direct fix for the
opencode/agy Windows flaking seen repeatedly during ensemble's own design session.)

### Testing
adapters need live CLIs → `#[ignore]` live smoke. conductor/blackboard/gate are hermetic via
a mock adapter returning canned `AgentOutput`. transport tested with a loopback "remote".

## 4. Reference projects (filled by the two running studies)

### 4a. Single-machine multi-CLI (deep-read of 8 repos — PENDING workflow wf_7e9831de-16f)
bernstein · cc-multi-cli-plugin · mcp-agent-switchboard · vibe-kanban · emdash ·
pal-mcp-server(clink) · antfarm · claude-council. → per-vendor adapter cheat-sheet, the
inter-agent-comms mechanism, orchestration + gate patterns. *(to be inserted)*

### 4b. Cross-machine over Tailscale (deep-read of 5 repos — PENDING)
wolfpack (Tailnet PTY broker) · grackle (SSH/gRPC) · yonder (git-spine + events.jsonl) ·
OpenHands (remote Agent Servers) · antfarm (file-queue across nodes). → the Tailscale
transport, cross-machine shared blackboard, node discovery/health/degrade. *(to be inserted)*

## 5. The path (达成路径 — phased)

- **Phase 0 — shell** (this folder): Rust + Apache-2.0 project; `Adapter` trait + a mock
  adapter; `crew.toml` schema; hermetic test harness. Everything below is testable without a
  live CLI via the mock.
- **Phase 1 — single-machine role pipeline (乙) + blackboard** ← the first real deliverable:
  implement(codex) → review(claude) → debug(agy) in one worktree, blackboard carrying
  inter-agent messages, quorum gate, flake degradation. **Goal: a working 4-CLI collaborative
  dev loop on one machine.**
- **Phase 2 — single-machine parallel tasks (甲)**: pipeline as a self-contained unit, run N
  in parallel; claim from a backlog.
- **Phase 3 — cross-machine over Tailscale**: remote adapter (drive an AI CLI on a worker
  node over the tailnet); cross-machine shared blackboard/state; node discovery +
  health + degrade-on-node-down. Grounded in §4b (wolfpack/OpenHands/grackle).
- **Phase 4 — governance hardening**: signed/hash-chained proofpack of verdicts; anti-
  anchoring blind review; ACP/MCP alignment so ensemble composes with the ecosystem.

## 6. Decisions (locked)
- Name **ensemble** · License **Apache-2.0** · Language **Rust** (single binary, cross-platform,
  reuse prior headless CLI adapters) · Config **TOML** · standalone repo.
- Scope: 甲 + 乙 + inter-agent comms + cross-machine-over-Tailscale.
- Differentiator stays the governance/cross-vendor/cross-machine/comms intersection.
