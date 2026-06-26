# Runbook — live MCP crew proof (Phase-1 step 4)

> **Goal.** Prove the keystone of Phase 1 end-to-end: a *live* AI CLI (claude / codex / opencode),
> acting as an MCP **client**, becomes a first-class crew member through the `ensemble mcp` server —
> reading the mesh + shared board, claiming work, getting its own git worktree, posting results,
> merging, and (optionally) delegating a whole governed sub-run. This needs the operator's machine(s)
> and real CLIs, so it can't be done in CI; this runbook is the script.

## What is already proven (autonomously, no operator needed)

- **169 hermetic unit/integration tests** green (`cargo test`), incl. the pure MCP `dispatch` for every tool.
- **An empirical end-to-end stdio smoke of the REAL `ensemble mcp` binary** (driven like an MCP client
  would, one JSON-RPC request per line, reading each response before the next) — 13/13 checks:
  `initialize` handshake · `tools/list` advertises all 10 tools · `board_post`→`board_read` round-trip ·
  `enqueue`→`claim`→`complete` work-queue · ownership-guarded `complete`/`fail` no-op on an unowned task ·
  `worktree` isolation under `--repo` · capability gating (`ensemble_run` advertised **iff** a `--crew`
  runner is wired; an unconfigured call → `-32603`).

So the **server side speaks MCP correctly over real stdio**. What this runbook validates is the missing
half: a **real CLI as the MCP client** driving that server, and **multiple** members coordinating.

## Prerequisites (per machine)

1. Build/install the binary: `cargo build --release` (binary at `target/release/ensemble`), or
   `cargo install --path .`.
2. `ensemble doctor` is green — it reports which AI CLIs + `tailscale` are on PATH and whether the cwd
   is a git repo. Fix any `MISSING` before continuing.
3. For the **multi-machine** parts: `tailscale up` on each node. ⚠️ If peers can't see each other, check
   the your VPN↔Tailscale WireGuard conflict (turn your VPN off, then `tailscale up`).
4. Work inside a **throwaway git repo** (not a repo you care about) — the crew creates branches +
   worktrees under `.ensemble/`. A scratch repo:
   ```bash
   mkdir /tmp/crewdemo && cd /tmp/crewdemo && git init && git commit --allow-empty -m init && git branch -M main
   cp /path/to/ensemble/examples/crew.toml .   # needed only for ensemble_run (Part 3)
   ```

---

## Part 1 — one live CLI as a crew member (single machine)

Register `ensemble mcp` as an MCP **stdio** server for your live CLI. The launch contract is always:

```
ensemble mcp --repo <repo> --name <UNIQUE-member-name> [--crew <crew.toml>]
```

`--name` is this member's identity for board posts + claims (server-set, never client-supplied → no
impersonation). `--crew` is needed only if you want `ensemble_run` (Part 3); without it the other 9
tools work and `ensemble_run` is simply not advertised.

**One-click (recommended) — `ensemble mcp install`** writes each CLI's MCP-server config for you, so you
never hand-edit per-client formats. From inside the repo:
```bash
cd /tmp/crewdemo
ensemble mcp install --client codex    --name codex-1    --crew crew.toml
ensemble mcp install --client claude   --name claude-1   --crew crew.toml
ensemble mcp install --client opencode --name opencode-1 --crew crew.toml
```
Each writes that CLI's REAL config — claude → project `.mcp.json`, opencode → project `opencode.json`
(per-call timeout 600000 so a long `ensemble_run` isn't killed), codex → user `~/.codex/config.toml`
(`$CODEX_HOME` honored). It DERIVES the ensemble binary path (`current_exe`) and the absolute repo/crew,
merges IDEMPOTENTLY (re-running just updates the entry, never duplicates), preserves your other MCP
servers + comments, and writes ATOMICALLY (no corruption/half-write on crash). Add `--print` to preview
the exact config without writing, or `--config <path>` to target a non-default file. `--name` defaults to
the client name; give a DISTINCT one per member. `--crew` is needed only for `ensemble_run` (Part 3).
Restart the CLI to pick it up, then drive it (below).

**Manual (if you prefer):** `claude mcp add ensemble -- /path/to/ensemble mcp --repo . --name claude-1
--crew crew.toml`, or add an `[mcp_servers.ensemble]` entry by hand. The only invariant is the launch
contract `ensemble mcp --repo <repo> --name <member> [--crew ...]`.

**Drive it.** In the live CLI session, prompt it to exercise the crew API, e.g.:

> "Use the ensemble tools: call `ensemble_mesh` and tell me the crew. Then `ensemble_board_post` a
> `plan` saying what you'll do. `ensemble_enqueue` the task "add a hello() to lib", `ensemble_claim` it,
> `ensemble_worktree` to get a workspace, make the edit + commit it in that worktree, then
> `ensemble_merge` its branch onto main and `ensemble_complete` the task."

**Verify the footprint (from a shell in the repo):**
```bash
cat .ensemble/board.jsonl          # the member's posts, attributed to its --name
ensemble ledger status --ledger .ensemble/ledger.db    # task: claimed → done, claimed_by=<member>
git branch                         # the ensemble/<member>/<task> branch (merged onto main)
git log --oneline -5 main          # the landed work
```

✅ **Pass:** a board post from `<member>`, a `done` ledger row claimed by `<member>`, and the edit on `main`.

---

## Part 2 — the multi-CLI crew (the real 4-CLI proof)

**Session = the repo.** Every `ensemble mcp` launched in the SAME repo shares `.ensemble/board.jsonl`
(blackboard) and `.ensemble/ledger.db` (work-queue), so different live CLIs coordinate through on-disk
state — no shared memory needed.

1. (Cross-machine, optional) On each node run `ensemble up` (prints the mesh, then serves on the
   tailnet IP). Confirm `ensemble mesh` on one node lists the other nodes + the agents they host.
2. Register `ensemble mcp` for **2–4 different live CLIs**, each pointed at the **same repo** with a
   **distinct `--name`** (e.g. `claude-1`, `codex-1`, `opencode-1`). Cross-machine: point each at the
   same repo on its node (a shared/synced checkout) — Phase-1 default is one shared on-disk repo.
3. Coordinate them by prompting:
   - One member: `ensemble_enqueue` three tasks.
   - Each member: loop `ensemble_claim` (at-most-once → no two members get the same task),
     `ensemble_worktree`, do the work + commit, `ensemble_merge`, `ensemble_complete` — or
     `ensemble_fail` with a reason if blocked.
   - A "reviewer" member: `ensemble_board_read` the others' `result` posts and `ensemble_board_post` a
     `verdict`.

**Verify:**
```bash
ensemble ledger status --ledger .ensemble/ledger.db   # 3 tasks done, each claimed_by a DIFFERENT member
cat .ensemble/board.jsonl                             # interleaved posts from multiple --names
git log --oneline --graph -15 main                    # all branches landed
```

✅ **Pass:** the three tasks are `done`, **each claimed by a different member** (proves at-most-once
across separate MCP processes), the board shows multi-member traffic, and all work is on `main`.

---

## Part 3 — delegate a whole governed sub-run (`ensemble_run`)

With `--crew crew.toml` wired (real agents on PATH / tailnet), have a live member call:

> "Use `ensemble_run` with task "implement and test a factorial function" and report the result."

`ensemble_run` blocks while the conductor runs the full **implementer → test-gate → reviewers → gate**
pipeline in its own throwaway worktree, then returns `{landed, rounds, branch}` (or `{landed:false,
rounds, reason}`). On land, the member lands the branch with `ensemble_merge`.

**Verify:** a per-run journal under `.ensemble/runs/<slug>.jsonl` (the full transcript + terminal
decision), and the landed branch.

✅ **Pass:** `ensemble_run` returns `landed:true` and the journal shows the multi-agent collaboration.

---

## Acceptance summary

Phase-1 step 4 is **proven** when, from real live CLIs, you have observed: (a) a single member round-trip
(Part 1), (b) multiple members coordinating at-most-once through the shared board + ledger (Part 2), and
(c) a delegated governed sub-run landing via `ensemble_run` (Part 3).

## Troubleshooting

- `ensemble_run` returns `-32603 "not configured"` or isn't listed → launch `ensemble mcp` **with
  `--crew <crew.toml>`** (a missing/unparseable crew.toml leaves the runner off; the other 9 tools still work).
- Peers invisible in `ensemble mesh` → `tailscale status`; resolve the your VPN↔Tailscale conflict.
- A claimed task stuck `claimed` → the member died before a terminal write; `ensemble ledger recover`
  requeues stale claims (or the member calls `ensemble_complete`/`ensemble_fail`).
- A merge reports `{landed:false, conflict:[...]}` → resolve in the member's worktree and retry, or use
  `ensemble merge <branch> --resolver <agent>` for one AI-resolver round.

## Note

During autonomous validation, a single non-reproducible anomaly was seen where one smoke run created its
worktree under the launching cwd's repo instead of `--repo`. It could **not** be reproduced under
controlled conditions (the binary was verified to honor `--repo` across cwd=repo, cwd≠repo, and
with/without `--crew`). If you ever see a worktree land outside `--repo`, capture the exact invocation —
a defensive "computed worktree base must be under `--repo`" assertion is queued as a hardening.
