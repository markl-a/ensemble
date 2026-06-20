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

## 4. Reference projects (source-grounded — deep-read of real code, 2026-06)

### 4a. Single-machine multi-CLI — what to borrow

**Inter-agent communication (the headline). Real mechanisms found in the wild:**
- **bernstein** (richest, and actually wired into the live spawn loop): (1) a **bulletin board
  summary injected into every spawned agent's prompt** ("Other agents are working in parallel.
  Recent activity:…"); (2) **agent-to-agent delegation by role** (post → query-by-role → claim
  → post-result); (3) direct file channels with `@mention` + a wakeup signal; (4) a real-time
  stdin-pipe IPC bus.
- **mcp-agent-switchboard**: a **debate** that relays one CLI's last text into the other's next
  prompt (each side resumes its own session id), plus a **SQLite shared-context blackboard** +
  event timeline, plus handoff-prompt-injection.
- **cc-multi-cli-plugin**: **none** — strict hub-and-spoke, everything mediated by Claude.
- **→ ensemble decision:** the **mediated blackboard whose rolling summary is injected into the
  next agent's prompt** (bernstein #1) is the cleanest fit for subprocess CLIs and is what §3's
  `blackboard/` implements. Add **role-delegation** (bernstein #2) as a follow-up. Skip the
  stdin-pipe IPC bus (fragile across vendors).

**Driving agy (3 different approaches observed — pick + verify):**
- bernstein: `antigravity -p <prompt> -m <model> --output-format json --yolo` as a plain
  subprocess, parse the logfile (treats agy as the Gemini-CLI rename).
- cc-multi: `agy -p --log-file <f>` with stdin closed + stdout ignored, then **read the on-disk
  transcript JSONL** (last `source=MODEL,type=PLANNER_RESPONSE` step; Windows `.tmp→.pb` retry).
- switchboard: drive the **IDE via remote-debugging** (no headless path).
- **→ ensemble's agy adapter:** try `--output-format json` first BUT **re-verify on the current
  agy** (a prior finding: on agy 1.0.10 the `-p` transcript.jsonl is empty and the data moved to
  a protobuf per-conversation `.db`; a real-PTY capture is the proven fallback). Adapter ships
  both paths with a capability probe.

**Orchestration + gate + config:** bernstein = worktree-per-task + an **objective janitor gate
(tests/lint, not cross-review)** + per-vendor adapter with a discovery cascade and a strategy
contract table; emdash = per-vendor provider defs (`cli`/flags/`terminalOnly`). ensemble keeps
the quorum *review* gate AND adds an objective test/lint gate (borrow bernstein) as a
non-LLM trust anchor.

### 4b. Cross-machine over Tailscale (Phase 3) — what to borrow

- **(a) Transport = wolfpack's `tailscale serve --bg <port>` fronting a loopback-only agent
  host** → reach a node at `https://<node>.<tailnet>.ts.net` with **zero open ports + free TLS +
  identity** (`tailscale-user-login` header). Address work by a **stable node-id and resolve the
  live tailnet URL at call time** (OpenHands' "discovered URL, not configured"). Live agent
  stream over WS with **`since_seq` ring-buffer replay on reconnect** (wolfpack) so a dropped
  link doesn't lose output. **Avoid** grackle's reverse-SSH tunnels — on a tailnet every node is
  already addressable.
- **(b) Cross-machine shared state = one SQLite/WAL coordination ledger** (node registry +
  tasks + `dispatch_queue UNIQUE(task_id)` for at-most-once — grackle), any vector/semantic store
  kept as a **disposable cache**. Overlay **yonder's contract**: **job-id == a ref-pinned
  `dispatch/<job-id>` git branch** (an agent can never touch `main`) + a **fsync'd terminal
  record as the ONLY completion signal** (silence ≠ success) — maps straight onto the
  flight-recorder. antfarm's **folder-as-status + atomic `rename`** is the DB-free alternative
  (valid only with a single writing process).
- **(c) Discovery/health/degrade:** `tailscale status --json` + an `/info` probe (wolfpack);
  **heartbeat → mark-disconnected → *suspend* (not lose) the session → backoff-reconnect →
  recover** (grackle); a dead node simply **stops pulling** work + its orphaned claims
  auto-recover on owner-liveness lapse (antfarm/yonder).
- **Cross-cutting — the ④ differentiator:** *every* repo studied hardcodes
  `bypassPermissions`/`full-auto`/`danger-full-access`. ensemble borrows their **dispatch +
  transport + completion-contract**, NOT their blank-cheque unattended posture — the governor +
  flight-recorder + quorum are exactly that gap.
- **Fleet caveats:** wolfpack's broker is **POSIX-only** (Windows nodes need TCP-loopback, not a
  Unix socket); yonder's Windows process-liveness check is a stub → ship a **real Windows
  liveness probe from day one** for Windows worker nodes.

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
  - **3a ✅** RemoteAdapter + `ensemble serve` agent-host + tailnet discovery (transport proven).
  - **auto-discovery ✅** `run/dispatch` default-ON probe each tailnet `serve` peer's `/health` and
    route any agent without an explicit `[agents.<n>] node` to the discovered host (explicit >
    discovered > local). `ensemble nodes` lists hosts; `--no-discover` opts out. No hand-written URLs.
  - **firewalls ✅ (swarm-hardening)** (A) automated TEST gate — `[test] command` must pass GREEN
    before a task lands, RED bounces the traceback to the implementer (`src/test_gate.rs`); (B)
    circuit breaker — `[gate] stall_limit` (no-progress early-break) + `max_task_secs` (wall-clock
    budget) + Ctrl-C clean abort. Spec/plan: `docs/specs|plans/2026-06-20-firewalls-*`.
  - **3b-1 ✅** cross-machine **git-sync via git bundles**: a remote agent runs on the
    orchestrator's base commit (bundled over the `/run` wire) and its edits flow back into the
    orchestrator's worktree — the Adapter abstraction holds, so the conductor + Phase-2c
    persistence are unchanged. (`src/repo_sync.rs`; plan `docs/plans/2026-06-19-phase3b1-*`.)
  - **3b-2 ✅ (slice-1)** SQLite coordination ledger (`src/ledger.rs`, rusqlite/WAL): enqueue
    at-most-once + atomic claim + yonder terminal-record + orphan recovery; durable resumable
    `ensemble dispatch` (`src/dispatch.rs`) + `ensemble ledger status|recover`. Plan
    `docs/plans/2026-06-20-phase3b2-*`.
  - **3b-2b** cross-machine shared ledger (serve workers PULL over HTTP / shared FS) + node
    registry/heartbeat + heartbeat-renewed leases → a true multi-node pull fleet.
- **Phase 4 — governance hardening**: signed/hash-chained proofpack of verdicts; anti-
  anchoring blind review; ACP/MCP alignment so ensemble composes with the ecosystem.

## 6. Decisions (locked)
- Name **ensemble** · License **Apache-2.0** · Language **Rust** (single binary, cross-platform,
  reuse prior headless CLI adapters) · Config **TOML** · standalone repo.
- Scope: 甲 + 乙 + inter-agent comms + cross-machine-over-Tailscale.
- Differentiator stays the governance/cross-vendor/cross-machine/comms intersection.
