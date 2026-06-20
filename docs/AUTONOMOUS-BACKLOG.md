# ensemble — autonomous dev backlog (ensemble-first)

**North-star:** finish `markl-a/ensemble` (the local-first GOVERNED cross-vendor cross-machine
AI-CLI collaborative dev crew), THEN advance the phantom-mesh main roadmap. This file is the
queue a recurring cron drains — **one double-gated task per tick.**

## Priority queue (work the top undone item)

1. [x] **Phase 3b-2 — SQLite coordination ledger (slice-1).** ✅ landed — `src/ledger.rs` (rusqlite/WAL):
   enqueue (idempotent/at-most-once), atomic claim, terminal complete/fail, recover_orphans; `src/dispatch.rs`
   durable resumable drain; `ensemble dispatch`/`ensemble ledger status|recover` CLI. Gate caught a stale-clock
   re-run bug (fixed). **3b-2b (next durable slice):** cross-machine shared ledger (serve workers PULL over
   HTTP/shared-FS) + node registry/heartbeat table + heartbeat-renewed leases (so a long live run isn't
   recovered) + per-state guards on complete/fail.
2. [ ] **Phase 3b-1 follow-ups (small; interleave when a tick is light):**
   - thin result bundles (`git bundle create - <branch> --not <base_sha>`; carry `base_sha` in `RepoCtx`).
   - true-merge apply (not `--ff-only`) for multi-round / dirty-worktree cases + a conflict policy.
   - [x] prune `refs/ensemble/*` after a successful ff-merge ✅@ecaecc8 (best-effort `update-ref -d`; round-trip test asserts the namespace is empty).
   - node scratch GC: sweep stale `ensemble-node-*` temp dirs on `serve` startup.
3. [ ] **Phase 3c — per-node status dashboard (各做各的):** read-only view of which node is running what
   (health + current job + last verdict), over the existing tailnet HTTP. Disposable cache, not a source of truth.
4. [ ] **Phase 4 — governance hardening:** signed / hash-chained proofpack of verdicts; anti-anchoring
   BLIND review (reviewer can't see prior verdicts); ACP/MCP alignment so ensemble composes with the ecosystem.
6. [ ] **Per-agent CLI config (operator-requested):** `[agents.<n>] model = "..."` / `args = [...]` / `program = "..."`
   / `timeout = N` in crew.toml — today the CLI binary/flags/model are hardcoded in ExecAdapter/AgyAdapter.
   Let crew.toml pick each AI's model + extra flags.
7. [ ] **Discovery hardening (gate follow-ups):** `discover_nodes()` runs `tailscale status --json` with NO
   subprocess timeout → a wedged tailscaled would hang every `run` (now default-on hot path) — add a bounded
   wait. + discovery port is hardcoded 7878; add `[discovery] port` / `--disc-port`. + MagicDNS-off fallback to
   `TailscaleIPs`. + parallelize peer probes.
5. [ ] **ensemble done → phantom-mesh main roadmap.** Switch repos. Pick the top item from the main
   roadmap (apex ② owned-memory phase-2, ④ governed unattended runs, etc.). Work in a **worktree off main** —
   NEVER disturb the dirty `feat/l1-governed-worker` tree. Scrub IPs/dates/machine-names/internal-paths before
   any public push.

## Loop protocol (every cron tick)
- Pick the single highest-priority undone item above.
- Brainstorm (if non-trivial) → durable plan → TDD implement → build/test via **WSL**
  (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`; native debug hits Defender LNK1104).
- Gate with **codex + claude** (`cmd //C codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check < prompt`
  / `cmd //C claude -p < prompt` — prompt via STDIN). **Only on ≥2 distinct-AI LGTM:** commit + `git push origin main`.
  If the gate finds issues, fix and re-gate. **Never fake-green.**
- Check off the item here (and add any discovered follow-ups); commit this backlog.
- Update memory if a milestone landed. **One task per tick, then stop** — the next tick continues.

## Guardrails (always)
- Double-gate: ≥2 different AIs must both LGTM before anything lands. Trivial mechanical edits exempt.
- Auto-merge ONLY gated work. ensemble = clean repo → direct-to-`main` is fine; phantom-mesh → worktree off main.
- Commit messages end with the `Co-Authored-By: Claude Opus 4.8 (1M context)` line. 繁體中文 in chat, English in code.
- Do NOT spawn additional autonomous loops. If blocked or a real decision needs the operator, STOP and leave
  a note in this file rather than guessing.

## Log (most recent first)
- 2026-06-20 — **firewalls (swarm-hardening)**: (A) automated TEST gate — project tests must pass GREEN before a task lands, RED bounces the traceback to the implementer; (B) circuit breaker — no-progress early-break (`stall_limit`) + wall-clock budget (`max_task_secs`) + Ctrl-C abort (clean stop at round boundary). `src/test_gate.rs` + `[test]`/`[gate]` config + conductor wiring. Part A double-gated (codex+claude, 5 review rounds — caught a real false-timeout bug, then simplified to drop the optional per-command timeout). Spec `docs/specs/2026-06-20-firewalls-*`, plan `docs/plans/2026-06-20-firewalls-*`. Follow-ups: B.3b true mid-call subprocess kill; robust test-command timeout (process-group/job-object); semantic (not byte-identical) stall detection. Deferred firewalls: lanes+phone-approval, container limits, failure-memory RAG, embedding log topology.
- 2026-06-20 — **default-on tailnet auto-discovery** @bc11a08 (operator-requested): `run/dispatch` auto-find tailnet `serve` hosts (probe `/health`) for any agent without an explicit `node`; explicit > discovered > local; `ensemble nodes` + `--no-discover`. Gate (codex) caught a bare-switch arg-parse bug → fixed. New items 6 (per-agent model/flags config) + 7 (discovery hardening) added.
- 2026-06-20 — 3b-1 follow-up: prune `refs/ensemble/*` after ff-merge @ecaecc8 (codex+claude LGTM). Remaining item-2 sub-tasks: thin bundles, true-merge, node scratch GC.
- 2026-06-20 — Phase 3b-2 SQLite coordination ledger (slice-1) landed: durable resumable `ensemble dispatch` + `ledger` CLI; gate (codex) caught a stale-batch-clock re-run bug → fixed.
- 2026-06-20 — Phase 3b-1 cross-machine git-sync landed @a775298 (this backlog created).
