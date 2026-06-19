# ensemble — autonomous dev backlog (ensemble-first)

**North-star:** finish `markl-a/ensemble` (the local-first GOVERNED cross-vendor cross-machine
AI-CLI collaborative dev crew), THEN advance the phantom-mesh main roadmap. This file is the
queue a recurring cron drains — **one double-gated task per tick.**

## Priority queue (work the top undone item)

1. [ ] **Phase 3b-2 — SQLite coordination ledger.** Durable node registry + `dispatch_queue UNIQUE(task_id)`
   (at-most-once dispatch) + a completion record that is the ONLY success signal (yonder: silence ≠
   success) + orphaned-claim recovery on node-liveness lapse. Turns the synchronous orchestrator-push
   into a durable pull-based backlog. New module `src/ledger.rs` (rusqlite, WAL). Grounded in design §4b(b/c).
2. [ ] **Phase 3b-1 follow-ups (small; interleave when a tick is light):**
   - thin result bundles (`git bundle create - <branch> --not <base_sha>`; carry `base_sha` in `RepoCtx`).
   - true-merge apply (not `--ff-only`) for multi-round / dirty-worktree cases + a conflict policy.
   - prune `refs/ensemble/*` after a successful ff-merge (the worktree branch already carries the commit).
   - node scratch GC: sweep stale `ensemble-node-*` temp dirs on `serve` startup.
3. [ ] **Phase 3c — per-node status dashboard (各做各的):** read-only view of which node is running what
   (health + current job + last verdict), over the existing tailnet HTTP. Disposable cache, not a source of truth.
4. [ ] **Phase 4 — governance hardening:** signed / hash-chained proofpack of verdicts; anti-anchoring
   BLIND review (reviewer can't see prior verdicts); ACP/MCP alignment so ensemble composes with the ecosystem.
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
- 2026-06-20 — Phase 3b-1 cross-machine git-sync landed @a775298 (this backlog created).
