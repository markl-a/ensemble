# ensemble

**A local-first, governed orchestrator that turns different-vendor AI coding CLIs into one
collaborative dev crew — across your own machines.**

`ensemble` is a single Rust binary that drives **Claude Code**, **OpenAI Codex**,
**Google Antigravity (`agy`)**, and **opencode** as a *crew*: one agent implements, others
review and audit, and work only lands when **two distinct vendors** sign off (a double-gate).
The crew can be spread across several machines on a Tailscale mesh, and every run is
**observable** (watch / steer / abort), **durable** (resumable via a SQLite ledger), and
**quota-resilient** (a rate-limited vendor is substituted by a different one, never faked
into a pass).

It is the standalone, open-source sibling of a larger personal-AI project. It works today: it
has run real `codex → test → claude → LANDED` rounds on a multi-node mesh.

---

## Why it's interesting

Plenty of tools shell out to one AI CLI. `ensemble` sits at an unusual **intersection**:

- **Cross-vendor governance** — a quorum gate that requires LGTM from *two different vendors*
  before a change merges, so no single model can rubber-stamp its own work.
- **Cross-machine** — route any role to a remote node over Tailscale; the mesh of `serve`
  hosts is auto-discovered, so you don't hand-write URLs.
- **Graceful degradation** — when a vendor flakes, empties, or hits a rate limit, it is
  retried, substituted with a backup vendor, or excluded from quorum **with a logged reason**.
- **Observable + durable** — a control plane (`watch` / `steer` / `abort`), a flight-recorder
  journal, and a resumable SQLite ledger for batch dispatch.

No single open-source tool combines all four today.

---

## Architecture

```
                          crew.toml  (pipeline + gate + roles + per-agent nodes)
                                 │
        ┌────────────────────────┴────────────────────────┐
        │                    conductor                     │
        │   implement ─▶ review ─▶ audit  (role pipeline)  │
        └───┬──────────────┬───────────────┬───────────────┘
            │              │               │
        ┌───▼───┐     ┌────▼────┐     ┌────▼────┐        each role → an Adapter:
        │ codex │     │ claude  │     │   agy   │          local CLI, or a
        └───┬───┘     └────┬────┘     └────┬────┘          remote node over Tailscale
            │ implements   │ reviews       │ audits / tests
            └──────────────┴───────┬───────┘
                                   │
                            ┌──────▼──────┐   blackboard: agents post
                            │ quorum gate │   "did X" / APPROVE / CHANGES
                            └──────┬──────┘   and read each other's notes
                  ≥2 DISTINCT      │
                  vendors LGTM ────┤───────▶ merge the worktree
                                   └───────▶ else: revise (next round) or escalate

  state:  worktree per task   ·   SQLite ledger (durable, resumable dispatch)
  mesh:   `ensemble serve` on each node   ·   auto-discovery picks who hosts which CLI
```

A typical round:

```
task → git worktree → codex implements + posts "did X" to the blackboard
                    → claude reads the diff + blackboard → APPROVE / CHANGES + a message
                    → on CHANGES, codex receives claude's note and revises (agents talk)
                    → agy runs the tests / audits + posts findings
                    → quorum gate (≥2 distinct vendors) → merge the worktree, or escalate
```

---

## Quickstart

**Prerequisites:** Rust (stable), the vendor CLIs you want to use on `PATH`
(`claude`, `codex`, `agy`, `opencode`), and — for cross-machine — Tailscale.

```sh
# 1. build
cargo build --release          # → target/release/ensemble

# 2. check this machine is ready (which CLIs + tailscale are on PATH, is-git-repo)
ensemble doctor

# 3. run one task through the crew in the current git repo
ensemble run "add a --version flag and a test for it" --crew crew-main.toml

# add --merge to land the kept worktree on success; --watch <role> to tail a live member
ensemble run "fix the flaky timeout test" --crew crew-main.toml --merge
```

### What a crew looks like

`crew.toml` declares the pipeline, the quorum gate, and which vendor fills each role.
The shipped `crew-main.toml`:

```toml
pipeline = ["implement", "review", "audit"]

[gate]
min_approvals = 2        # quorum needs TWO distinct reviewer vendors
max_rounds    = 2
on_flake      = "exclude" # a flaky vendor is dropped from quorum, with a logged reason

[test]
command = "cargo test --quiet"

[roles.implement]
agent = "codex"

[roles.review]
agent = "claude"
blind = true

[roles.audit]
agent = "agy"
blind = true

# Quota resilience: on a rate-limit, a role's work is handed to a DIFFERENT vendor once.
[agents.codex]
timeout = 60
backup  = "agy"

[agents.claude]
timeout = 60
backup  = "agy"

[agents.agy]
timeout = 180
```

Verify how a crew parses before relying on it:

```sh
ensemble crew inspect --crew crew-main.toml --json
```

### Cross-machine (Tailscale mesh)

Run `ensemble serve` on each machine, then route a role to it from `crew.toml`:

```sh
# on node-a (a second machine on your tailnet)
ensemble serve            # binds this node's tailnet IP:7878 by default
# or:  ensemble up        # show the mesh, then serve in the foreground
```

```toml
# pin a specific role to a remote node (explicit node = always wins over discovery)
[agents.claude]
node = "http://node-a:7878"
```

By default `run` / `run-many` / `dispatch` **auto-discover** tailnet `serve` hosts and route
roles to whichever node offers the needed CLI, falling back to the local CLI when none does.
Pass `--no-discover` to stay local.

```sh
ensemble nodes            # probe the tailnet for serve hosts + the agents they offer
ensemble mesh             # this node's CLIs + which agents each peer hosts
```

---

## CLI surface

| Command | What it does |
|---|---|
| `run "<task>" [--crew] [--merge] [--watch <role>]` | Run one task through the crew pipeline |
| `run-many "<t1>" "<t2>" ...` | Run several tasks in parallel |
| `dispatch "<t1>" ... --ledger <db>` | Durable, **resumable** batch dispatch |
| `ledger <status\|recover> --ledger <db>` | Inspect / recover a dispatch ledger |
| `agent <name> "<task>" [--node auto\|<host>]` | Delegate ONE turn to one CLI |
| `all "<prompt>"` | Council: fan one prompt to every reachable CLI, side-by-side |
| `merge <branch> [--into <t>] [--resolver <agent>]` | Land a kept branch; conflicts escalate |
| `serve` / `up` | Run / quick-start this node's tailnet service |
| `nodes` / `mesh` | Discover serve hosts and which agents each offers |
| `mcp` / `mcp install` | Expose ensemble as an MCP server (make a live CLI a crew member) |
| `watch <member[@node]>` | Tail a live member's stream feed |
| `steer <member> "<prompt>"` | Inject a redirect into a live run's next round |
| `abort <member> [--hard]` | Stop a live run (`--hard` kills the CLI now) |
| `team <status\|say\|inbox>` | Inspect / post to the team blackboard |
| `supervise <name>` | Ask an AI to inspect recent run evidence |
| `doctor` | Check this machine is crew-ready |
| `crew inspect` | Print parsed crew/gate/reviewer metadata for verification |

Run `ensemble help` for the full, authoritative usage.

### MCP server

`ensemble mcp` runs a stdio MCP server that makes a **live** vendor CLI a first-class crew
member — it exposes the mesh, blackboard, work queue, worktree, merge, and run as MCP tools.
`ensemble mcp install --client <claude|codex|opencode>` registers it into that CLI's config in
one step.

---

## Status

**Active development.** The single-machine pipeline, parallel/durable dispatch, the cross-machine
mesh, the MCP server, and the control plane all work and have driven real end-to-end rounds on a
multi-node mesh. Interfaces may still change. Design notes live in
[`docs/2026-06-19-ensemble-design.md`](docs/2026-06-19-ensemble-design.md) and the `docs/RUNBOOK-*`
files.

## License

[Apache-2.0](LICENSE).
