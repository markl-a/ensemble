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
   - [x] node scratch GC ✅@59e1ea6 (codex+claude LGTM): `serve()` startup (AFTER bind) sweeps `ensemble-node-*`
     dirs left by a crashed/killed serve. PID-LIVENESS based (not age) — removes only dirs whose embedded owner
     pid is provably dead; `pid_alive` strictly fail-safe (uncertainty → keep). Pure `orphan_scratch`/`scratch_pid`
     unit-tested with mock liveness.
3. [ ] **Phase 3c — per-node status dashboard (各做各的):** read-only view of which node is running what
   (health + current job + last verdict), over the existing tailnet HTTP. Disposable cache, not a source of truth.
4. [ ] **Phase 4 — governance hardening:** signed / hash-chained proofpack of verdicts; anti-anchoring
   BLIND review (reviewer can't see prior verdicts); ACP/MCP alignment so ensemble composes with the ecosystem.
6. [ ] **Per-agent CLI config (operator-requested):** `[agents.<n>] model = "..."` / `args = [...]` / `program = "..."`
   / `timeout = N` in crew.toml — today the CLI binary/flags/model are hardcoded in ExecAdapter/AgyAdapter.
   Let crew.toml pick each AI's model + extra flags.
7. [x] **Discovery hardening** ✅@79a69f5 (codex+claude LGTM): bounded `tailscale status --json`
   (`capture_bounded`, 4s wall-clock + kill-on-timeout, exit-status-gated) so a wedged tailscaled never hangs the
   default-on `run`/`agent`/`nodes` path; parallel `/health` probes (`probe_all` via `thread::scope`, spawn-order
   join keeps first-host-wins) instead of a 2s×N serial wait; MagicDNS-off fallback to the stable `TailscaleIP`
   (`Node::endpoint()`, IPv4 preferred, IPv6 bracketed) so a second VPN mangling DNS doesn't blind discovery. The
   re-gate (codex from an EMPTY temp cwd — see lesson @f1811f8) caught 3 real bugs — IPv6 URL brackets, unbounded
   reap, dropped exit-status gate — all fixed before landing. **Remaining (small):** `[discovery] port` /
   `--disc-port` knob (port still hardcoded 7878).
8. [x] **`ensemble doctor`** — env-readiness check ✅@fab543e (codex+claude LGTM). Pure core
   `check_tools`/`is_ready` (hermetic, 5 tests) + thin IO `run_checks` (PATH probe via `where`/`command -v`,
   git-repo via reused `repo_sync::is_git_worktree`); `ensemble doctor` prints a report, exits non-zero when
   not ready (no git repo OR zero CLIs) so a script can gate. Built off a fresh `feat/doctor-v2` (the old
   `feat/doctor` branch predated F1 and would have reverted it). Gate caught a DRY dup (private git-repo helper
   → reuse exported one) → fixed.
9. [ ] **thin result bundles (F2):** 3b-1 perf — ship `git bundle create - <branch> --not <base_sha>` deltas;
   carry `base_sha` in `RepoCtx`. Not started.
10. [ ] **`ensemble agent` streaming (HIGH — operator wants live token streaming):** make a delegated `agent`
   turn STREAM the remote CLI's output live (adapter incremental read → serve chunked/SSE → RemoteAdapter
   forward → `agent` stdout, watched via Claude Code Bash/Monitor). Effort M-L; operator said "方向對但先別動手"
   — PENDING explicit design-approval before building.
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
- 2026-06-20 — **Discovery hardening LANDED @79a69f5** (codex+claude LGTM; item 7 done). The stashed item-7 draft (stash@{0}) was re-implemented as its OWN gated change on `feat/discovery-hardening`: `capture_bounded` runs `tailscale status --json` under a 4s wall-clock timeout (kill-on-timeout, exit-status-gated) so a wedged tailscaled can't hang the default-on hot path; `probe_all` probes peers' `/health` in parallel (`thread::scope`, first-host-wins preserved); `Node::endpoint()` falls back to the stable TailscaleIP (IPv4 preferred, IPv6 bracketed) when MagicDNS is off. **Re-gate done SAFELY** — codex ran from an EMPTY temp cwd (not the repo) after the f1811f8 gate-mutation incident, and it caught 3 real bugs (IPv6 URL brackets, unbounded `child.wait()` reap, dropped exit-status gate) which were fixed before landing. Work committed to a branch FIRST so a rogue gate couldn't lose it. ⚠️ A leftover `codex.exe` from the prior incident was still reverting the working tree mid-session → had to `taskkill /F`; `TaskStop` only killed the shell pipeline. Remaining: `[discovery] port` knob (item 7 sub-bullet).
- 2026-06-20 — **node-scratch GC LANDED @59e1ea6** (codex+claude LGTM; item 2 sub-task done). `serve()` startup sweeps `ensemble-node-*` dirs orphaned by a crashed/killed serve, AFTER bind. Design driven entirely by the gate: started age-based → codex flagged it could delete a LIVE long-running job's dir → reworked to PID-LIVENESS; then 3 more codex rounds closed fail-safe holes (/proc-missing reads all-dead, kill-0 EPERM read as dead, /proc-self authoritative check, tasklist success-gated). Pure `orphan_scratch`/`scratch_pid` mock-tested. ⚠️ DISCOVERED: a `codex exec` gate run silently EDITED `src/discovery.rs` (233 lines of item-7 discovery hardening) during a "review" — caught via `git status` (claude's review also flagged the unrelated diff). Stashed it out (stash@{0}, UNVETTED) so only the gated GC change landed; item 7 now has a draft to review+gate next tick. Lesson recorded in item 7.
- 2026-06-20 — **F3 `ensemble doctor` LANDED @fab543e** (codex+claude LGTM). Env-readiness check: reports the 4 AI CLIs + tailscale on PATH + is-cwd-a-git-repo, exits non-zero when the mesh can't run here (no git repo OR zero CLIs) so a script can gate (`ensemble doctor && ensemble run …`). Pure core `check_tools`/`is_ready` (5 hermetic tests) + thin IO `run_checks`. ⚠️ Did NOT reuse the stalled-batch `feat/doctor` branch — it predated F1 and its diff would have reverted F1's adapter.rs/main.rs work; salvaged just the scaffold + tests onto a fresh `feat/doctor-v2` off current main. Gate (claude) caught a DRY dup — a private `cwd_is_git_repo` duplicating the exported `repo_sync::is_git_worktree` → reuse the exported helper; codex LGTM'd round 1 (even built+ran it natively). Remaining open: F2 thin-bundles (item 9), agent streaming (item 10, pending operator design-approval), per-agent config (item 6), discovery hardening (item 7).
- 2026-06-20 — **F1 `ensemble agent` delegate verb LANDED @dfd46aa** (codex+claude LGTM). The interactive-conductor primitive: `ensemble agent <name> "<task>" [--node auto|<host>] [--repo] [--json]` → delegate ONE turn to a CLI (local or remote via discovery, edits land in --repo via git-sync); resolve_one returns (adapter,label); distinct exit codes per failure kind. Gate caught: >2-positional drop, `--node --json` value-swallow, inconsistent JSON node label — all fixed.
- 2026-06-20 — ⚠️ **parallel worktree-build workflow STALLED** (lesson): the 3-agent Workflow (F1/F2/F3 each in an isolation:'worktree' agent doing TDD+WSL-build+push) made file edits but **never committed/built/pushed** — the agents stalled mid-TDD (wrote failing tests, didn't implement). Salvaged F1 manually (the agent's exit_code + tests were good) + finished it. F3 `ensemble doctor` partial scaffold preserved on branch `feat/doctor` (finish a future tick). F2 thin-bundles not started. **TAKEAWAY: parallel-agent Rust-build-in-worktree is unreliable here; prefer orchestrator-implements + parallel GATES, or hand agents smaller non-build tasks.**
- 2026-06-20 — **firewalls (swarm-hardening)**: (A) automated TEST gate — project tests must pass GREEN before a task lands, RED bounces the traceback to the implementer; (B) circuit breaker — no-progress early-break (`stall_limit`) + wall-clock budget (`max_task_secs`) + Ctrl-C abort (clean stop at round boundary). `src/test_gate.rs` + `[test]`/`[gate]` config + conductor wiring. Part A double-gated (codex+claude, 5 review rounds — caught a real false-timeout bug, then simplified to drop the optional per-command timeout). Spec `docs/specs/2026-06-20-firewalls-*`, plan `docs/plans/2026-06-20-firewalls-*`. Follow-ups: B.3b true mid-call subprocess kill; robust test-command timeout (process-group/job-object); semantic (not byte-identical) stall detection. Deferred firewalls: lanes+phone-approval, container limits, failure-memory RAG, embedding log topology.
- 2026-06-20 — **default-on tailnet auto-discovery** @bc11a08 (operator-requested): `run/dispatch` auto-find tailnet `serve` hosts (probe `/health`) for any agent without an explicit `node`; explicit > discovered > local; `ensemble nodes` + `--no-discover`. Gate (codex) caught a bare-switch arg-parse bug → fixed. New items 6 (per-agent model/flags config) + 7 (discovery hardening) added.
- 2026-06-20 — 3b-1 follow-up: prune `refs/ensemble/*` after ff-merge @ecaecc8 (codex+claude LGTM). Remaining item-2 sub-tasks: thin bundles, true-merge, node scratch GC.
- 2026-06-20 — Phase 3b-2 SQLite coordination ledger (slice-1) landed: durable resumable `ensemble dispatch` + `ledger` CLI; gate (codex) caught a stale-batch-clock re-run bug → fixed.
- 2026-06-20 — Phase 3b-1 cross-machine git-sync landed @a775298 (this backlog created).
