# ensemble — autonomous dev backlog (ensemble-first)

**North-star:** finish `markl-a/ensemble` (the local-first GOVERNED cross-vendor cross-machine
AI-CLI collaborative dev crew), THEN advance the phantom-mesh main roadmap. This file is the
queue a recurring cron drains — **one double-gated task per tick.**

## ▶ CURRENT FOCUS (2026-06-21) — finish Phase 1 of the two-phase plan
Spec: `docs/specs/2026-06-20-two-phase-real-tests-design.md` + `docs/specs/2026-06-20-ensemble-mcp-design.md`.
DONE this run: step 1 merge ✅ · step 2 journal ✅ · **step 2b AI-resolver ✅ (merge_with_resolver + `ensemble merge --resolver`)** · step 5 exec-timeout ✅ · **step 3 `ensemble mcp` slice 1 ✅** (mesh + board_read) · **slice 2 ✅@3b5a96d** (`ensemble_board_post`) · **slice 3a ✅@f5f82fa** (`ensemble_worktree`; 8 gate rounds) · **slice 3b ✅@3ecf1c0** (`ensemble_enqueue`+`ensemble_claim` work-queue) · **slice 4a ✅@0121465** (`ensemble_merge {branch, into?}` — land a member's branch via `repo_sync::merge_branch`; conflict = reported outcome not error; concurrent merges serialized by a per-repo lock on the common .git dir; **4 gate rounds** — codex peeled back ref-confusion layer by layer (path `into:"f"` → leading-dash `--detach` → special-ref `HEAD`/tag-shadow), fixed with leading-`-` reject + `git rev-parse --symbolic-full-name == refs/heads/<name>`; codex+claude LGTM) · **slice ④b-i ✅@e6e25a8** (`ensemble_complete {id, outcome}` + `ensemble_fail {id, reason}` — ownership-guarded ledger terminal records: `ledger::complete_owned`/`fail_owned` do a SINGLE atomic guarded `UPDATE ... WHERE id=? AND state='claimed' AND claimed_by=?`, so a member can only close out a task it actually holds a live claim on — the `state='claimed'` predicate also kills the orphan-requeue race; a blocked write is a reported OUTCOME `{completed:false, detail}` not an error; codex+claude LGTM first round) · **slice ④b-ii ✅@d5f6208** (`ensemble_run {task}` — delegate ONE task to a HEADLESS governed crew sub-run via a `CrewRunner` trait injected into `Ctx` (the binary wires a `ConductorRunner` over `Conductor::run_in_repo`; mcp.rs stays free of crew/adapter wiring → happy path is hermetic via a FakeRunner); runs in `ctx.repo`, never a client path; returns `{landed,rounds,branch}`/`{landed,rounds,reason}`; **2 gate rounds** — codex CHANGES r1: tools/list advertised the tool even when production startup leaves the runner `None` (missing crew.toml) → fixed to advertise `ensemble_run` IFF `ctx.runner.is_some()`; r2 codex+claude LGTM). **MCP crew-participation API is now CODE-COMPLETE — all 10 tools** (mesh · board_read · board_post · worktree · enqueue · claim · merge · complete · fail · run).
**NEXT (resume here):** Phase-1 is CODE-COMPLETE. **Step 4 — live proof: LOCAL governed run on z13 ✅ DONE (2026-06-21).** A real `ensemble run` drove **codex (implement) → claude + agy double-gate (2/2 LGTM) → LANDED**, then `ensemble merge` landed the gated branch onto main (GREETING.txt proof, scratch repo, release binary, 1m43s). So the conductor's AUTOMATED implement→cross-vendor-gate→land pipeline is proven end-to-end with real CLIs. **Reliability finding ([[fleet-cli-reliability-governed-runs]]):** codex/claude are the reliable verdict-emitters; **agy works as a reviewer (~13s)**; **opencode HANGS as a headless ExecAdapter reviewer (600s timeout × 2 = ~23min wasted) → keep it OUT of headless governed roles until item 6 (per-agent timeout) lands.** Remaining for step 4: the CROSS-MACHINE 4-CLI proof over the tailnet (needs operator to bring up Tailscale: Surfshark off → `tailscale up` on each box; Mac binaries built on-Mac). OSS onboarding tick C is now code-complete; the autonomous queue resumes at **priority item 0 / tick A** — crates.io + GitHub Releases prebuilt binaries (cargo-binstall convention), while the operator-facing Phase 2 path still needs the real 5-node Slice C/D service smoke. Build pattern: TDD on pure handlers, double-gate (codex from an empty temp cwd + claude via stdin), **commit-to-branch-first with `git add <specific files>` (NEVER `git add -A` — [[git-add-A-sweeps-stray-build-dirs]])**, ff to main. WSL: `cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`. Deferred minors: (a) `dispatch::task_id` → content-stable hash (durable idempotency across toolchain upgrades); (b) MCP optional-arg schemas (`task`,`since`,`into`) accept `null`=default — future schema-consistency pass; (c) complete/fail close-out is not idempotent on retry — a member that re-issues an identical complete after success gets `{completed:false}` (already-terminal) which could read as failure (gate non-blocking note); (d) a symmetric `fail_owned` double-fail test for parity.

## Priority queue (work the top undone item)

0. [active] **OSS onboarding — "install → just works"** (operator-driven; spec
   `docs/specs/2026-06-20-oss-onboarding-design.md`). Goal: after `cargo install ensemble` + `ensemble up`,
   the tool auto-recognizes local + tailnet AI CLIs and orchestrates them — no manual Taildrop/serve/firewall.
   Trust = tailnet network-trust. Sequence **E→D→B→C→A**, each a double-gated tick:
   - [x] **E** serve binds the node's tailnet IP by default ✅@ee09598 (loopback fallback, never 0.0.0.0;
     `--bind` override). Empirically verified: tailnet IP reachable, loopback refused.
   - [x] **D** `ensemble mesh` ✅@50a0909 (codex+claude LGTM): `discover_mesh` (host→agents) +
     `present_clis` + pure `render_mesh`. Empirically prints local CLIs + tailnet hosts in ~1.5s.
   - [x] **B** `ensemble up` ✅@58e348b (codex+claude LGTM): resolve bind (E) → print mesh (D) → serve
     foreground until Ctrl-C. Pure `mesh::render_up` unit-tested; empirically verified the banner on z13.
   - [x] **C** `serve --install-service`/`--uninstall-service` ✅: Win schtasks / mac launchd user agent / Linux systemd user unit, with `--print` dry-run for safe verification. Pure generators cover command/unit/plist content; CLI helper refuses to silently bake ambient `ENSEMBLE_TOKEN` into persistent service state.
   - [ ] **A** distribution — crates.io + GitHub Releases prebuilt binaries (cargo-binstall convention).
0.5. [active — **S0+S1a+S1b+item-6 ✅ — 看得到/中斷/調整/設定 ALL live on Win+Mac; cross-machine control (S2/S3) next**] **LIVE cross-session supervision (主控台).** Let the
   operator's MAIN session monitor + steer OTHER AI-CLI sessions in real time: detect drift, inject an ad-hoc
   prompt, or force-abort. **Spec `docs/specs/2026-06-21-live-supervision-design.md`** (two planes: a coordination
   file-plane `.ensemble/stream|control/<member>.ndjson` + an enforcement `SessionBackend`; **M-API** for
   codex/opencode (structured server channel, no Windows ConPTY), **M-PTY** for claude/agy (raw PTY proxy)).
   **Slices: S0 ✅@7f1ded4** (codex+claude LGTM, 1 re-gate round — the read-only STREAM file-plane:
   `ndjson::Feed` append-only multi-process feed (mirrors board.rs locking) + `StreamEvent` schema (serde
   `"ev"`-tagged, forward-compat raw fallback) + `ensemble watch <member> [--repo][--since][--follow]` tail;
   member name confined to one component via shared `journal::sanitize_slug`; gate caught 2 real blockers —
   interior-newline ndjson record-fracture + watch stdout-error/BrokenPipe propagation — both fixed & re-gated).
   **S1a ✅@ad7a6d8** (codex+claude LGTM, 1 re-gate round — the **看得到 / observe** half: the conductor mirrors
   EVERY blackboard post into `.ensemble/stream/<name>.ndjson` via a `RunObserver` trait + `FeedObserver`
   (best-effort, never changes a run) injected by `ensemble run --watch <name>`; a single `note()` funnel
   replaces every `bb.post` in `run()` + streams the terminal decision; `ensemble watch` now renders BOTH
   member-session `StreamEvent`s AND governed-run `Message`s. Gate caught a real forward-compat render bug —
   an unknown `"ev"`-tagged line fell through to `Message` render; fixed to gate Message-render on absence of
   the `"ev"` key. Smoke: `ensemble run --watch s1demo` → `ensemble watch s1demo` shows result/verdict/decision
   live). **Follow-ups (non-blocking, from the gates):** (a) `run_in_repo`'s commit-failure downgrade
   (Landed→Escalated) is posted OUTSIDE `run()` so it isn't streamed — a watcher sees "LANDED" then the
   transcript downgrades; mirror it in a later sub-slice; (b) `run_many` + `--watch` share one feed (lossless
   but interleaved) — fine until run_many streaming is wired. **S1b next** (CONTROL plane: `.ensemble/control/
   <member>.ndjson` + `ensemble steer`/`abort` + serve `/stream`+`/control` routes) → **S2** ApiChannel
   (opencode→codex) → **S3** PtyProxy (claude/agy; mac first, Windows ConPTY last).
   **S1b ✅@cfc1760** (codex+claude LGTM, FIRST round — the CONTROL half / 中斷+調整): `ControlCmd`
   (`Steer`/`Abort{hard}`) on a per-run control feed `.ensemble/control/<name>.ndjson`; the conductor
   consumes steers (injected into the NEXT round's prompt) + aborts (clean = round-boundary stop; the
   adapters watch a `hard` flag so `--hard` kills the running CLI MID-TURN via the existing exec/PTY poll
   loops); `ensemble steer <name> "<prompt>"` + `ensemble abort <name> [--hard]` drive it; a daemon watcher
   feeds the run's `ControlState` (cursor from feed END, ignores stale). Smoke: `--hard` killed a live codex
   mid-essay. **Local control-plane follow-ups:** (a) a hard-abort that kills the IMPLEMENTER returns via the
   "implementer failed" arm so no `operator/interrupted` post is streamed (observability gap); (b) steer text
   also reaches reviewer prompts (shared `feedback`); (c) `abort_cmd` accepts extra positionals. NEXT: the
   **設定 / configure** half = backlog item 6 (per-agent `model`/`args`/`timeout` in crew.toml), then cross-
   machine control (S2/S3 ApiChannel/PtyProxy). **Analysis (2026-06-21):** full live observe/inject/interrupt is only possible for
   sessions ensemble SPAWNS (`run`/`agent`/serve sub-runs — ensemble owns the child stdio); a CLI acting as an
   MCP CLIENT (a live Claude Code) is COOPERATIVE-only, because MCP is client-initiated — the server cannot
   preempt a client's in-flight turn (best case: it polls the board between steps). So supervised members should
   prefer the spawn/serve topology. Design that fits the CURRENT arch + already-landed primitives: (a) **stream**
   — ExecAdapter's existing stdout/stderr reader threads also append to `.ensemble/stream/<member>.ndjson`
   (this IS backlog item 10 streaming); (b) **control file** `.ensemble/control/<member>.json` `{seq,inject?,
   abort?}` — the adapter's existing WRITER thread consumes `inject` → child stdin; a light poll thread consumes
   `abort` → the landed `kill_tree` (firewall-B's kill path, file-triggered instead of timeout = immediate
   force-stop); (c) thin subcommands `ensemble watch/steer/abort <member>` the main session drives via
   Bash/Monitor; cross-machine via serve `/stream` (SSE) + `/control` (POST). **Subsumes item 10.** Effort M;
   gate per slice (stream → control → subcommands). Today's zero-code coarse supervision (between-turn):
   `tail -f .ensemble/board.jsonl` + `.ensemble/runs/<slug>.jsonl` + `ensemble ledger status`; steer via
   `ensemble_board_post kind=steer`; Ctrl-C a governed run (clean stop at round boundary).
0.6. [x] **Auto member-name default** ✅@a8e7d69 (codex+claude LGTM **first round**). `ensemble mcp install`
   defaults `--name` to `<client>@<short-hostname>` instead of bare `<client>`, so members across the fleet
   (z13/ayaneo/acer/m1/m5) don't collide with ZERO coordination, and the name is STABLE across restarts —
   ledger claim-ownership + orphan-recover key on a fixed identity, so it's deterministic per (client, host),
   never a per-launch counter. Pure `mcp_install::default_member_name` (sanitizes the host's first label to
   `[a-z0-9_-]`, lowercases; None/empty/all-stripped → bare client) + IO `main::raw_hostname`
   (`COMPUTERNAME`→`HOSTNAME`→`hostname` cmd, no shell). Explicit `--name` still wins. Same-cli-same-host
   multi-open still takes an explicit `--name` (future: "smallest free `-N`" board probe). 3 TDD tests.
0.7. [queued — operator-requested 2026-06-21] **`ensemble all "<prompt>"` — COUNCIL broadcast (read-only).**
   Fan the SAME user prompt to EVERY AI CLI ensemble can see (local CLIs on PATH + every agent on every
   discovered tailnet peer), run each as ONE read-only turn (like `ensemble agent` but to ALL), and render
   every reply side-by-side. NO worktree, NO gate, NO land — pure council / compare-all-vendors-on-one-question
   (e.g. `ensemble all "這段架構有什麼風險?"` → codex@z13 / claude@m5 / opencode@ayaneo / agy@z13 each answer).
   Operator chose **Council** over the two other broadcast semantics, which stay as POSSIBLE later modes:
   *Tournament* (every CLI does the same task in its own worktree → judge/gate picks/merges best → land ONE)
   and *Broadcast-steer* (inject one operator prompt into EVERY live supervised session — extends 0.5/S1
   `steer <member>` to `--all`). Design: enumerate `(agent, node)` targets via `discover_mesh` + `present_clis`
   (the SET = everything discovered, not just crew roles); per target build an adapter by the SAME resolution as
   `adapters_for` (explicit node > discovered > local); run each `adapter.run(prompt, repo)` in parallel
   (`thread::scope`); collect `AgentOutput`; render a labeled block per `agent@node` (+ `--json`). Pure core:
   target enumeration + render (TDD'd); IO shell: the parallel fan-out. `--no-discover` stays local-only; a
   flaked CLI shows as `[flaked: …]`, never a hard error. Effort S–M; one double-gated slice. Builds directly on
   the landed F1 `ensemble agent` + the discovery substrate (item 7).
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
6. [x] **Per-agent CLI config (operator-requested)** ✅@a95e2d6 (codex+claude LGTM, FIRST round — the 設定
   verb of 0.5). `[agents.<n>] args = [...]` (extra flags appended to the LOCAL invocation, vendor-agnostic —
   `["--model","x"]` selects a model) + `timeout = N` (per-command seconds, overrides the adapter default).
   `AgentConfig` gains `args`/`timeout` (serde-default → old crew.toml unaffected) + `args_for`/`timeout_for`
   accessors; `ExecAdapter::with_extra_args`; `adapters_for`'s LOCAL branch applies them via `cfg_exec`/`cfg_agy`
   (remote nodes untouched). Smoke: `[agents.codex] timeout = 2` flaked codex in ~2s. Deferred: agy `args`
   (special argv), a dedicated `model`/`program` field (args subsumes model), a warn when agy `args` is set.
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
- 2026-06-25 — **Phase 2 fleet acceptance reports now bind generated crew hashes.**
  `scripts/phase2-fleet.ps1 -RunSelected -VerifyEvidence` now refreshes selected generated
  crew files from the current manifest before running, and acceptance reports include the
  manifest-derived generated crew SHA-256. `-VerifyReports` compares that hash against the
  current manifest-generated crew content, so a report from an older route/quorum plan cannot
  satisfy Slice C after the manifest changes. Verified with `phase2-fleet.ps1 -SelfTest`.
- 2026-06-25 — **Phase 2 Slice A now verifies member@node control routing end to end.**
  `scripts/phase2-verify.ps1` still tests explicit `--node <loopback-serve>` control
  routes, and now also drives `watch`, `steer`, and `abort` through
  `<name>@127.0.0.2:<port>` without `--node`. The verifier uses a non local-escape
  loopback alias and wrong-token mutation checks, so a local fallback would fail instead of
  silently writing the local file plane. This also adds a regression for `member@host:port`
  suffix routing so ephemeral loopback serve ports work without the unsafe `member@http://...`
  form. Verified with `cargo test --bin ensemble control_node_url_preserves_host_port_nodes`
  and Slice A only: `phase2-verify.ps1 -SkipSliceB -SkipSliceC -SkipSliceD`.
- 2026-06-25 — **Phase 2 local readiness now includes the cross-machine governance regression.**
  `scripts/phase2-local-ready.ps1` now runs `cargo test --test cross_machine --target-dir <target>`
  by default after Phase 1 acceptance, Phantom bridge, and Phase 2 Slice A/B-preflight/C-local.
  This makes the in-process remote `serve`/`RemoteAdapter` path, test-gate fail-stop behavior,
  two-distinct-reviewer quorum, and duplicate-vendor negative case part of the standard
  one-machine gate before the operator moves to the real m1~m5 fleet. Pass
  `-SkipCrossMachineRegression` only for fast troubleshooting.
- 2026-06-25 — **Phase 2 fleet acceptance reports can now be verified from the manifest.**
  `scripts/phase2-fleet.ps1 -VerifyReports -RepeatCount 2` reads the same fleet manifest and
  validates the selected projects' `acceptance-<project>-<node>.json` files. It checks `ok=true`,
  `verifyEvidence=true`, matching project/team/watch metadata, at least the expected repeat count,
  and verified `landed|escalated` run entries. `-Node all -VerifyReports -RepeatCount 2` gives the
  real Slice C pass a single manifest-driven report audit when one host can read all repos; otherwise
  each node can verify its own selected project. Verified with `phase2-fleet.ps1 -SelfTest`.
- 2026-06-25 — **Phase 2 fleet selected runs now write acceptance reports.**
  `scripts/phase2-fleet.ps1 -RunSelected -VerifyEvidence -RepeatCount 2` now writes
  `<repo>/.ensemble/phase2-fleet/acceptance-<project>-<node>.json` next to the generated
  crew after all repeats pass. The report records the selected project, node, quorum metadata,
  repeat count, terminal result, exit code, team/watch/control cursors, and evidence-verifier
  status for each run. Selected runs clear old acceptance reports up front, so a later failed
  run cannot leave a stale green report behind. This gives the real m1~m5 Slice C pass a
  machine-readable handoff artifact instead of relying on terminal scrollback. Verified with
  `phase2-fleet.ps1 -SelfTest`.
- 2026-06-25 — **Phase 2 fleet selected runs can now verify rerunability.**
  `scripts/phase2-fleet.ps1 -RunSelected` now accepts `-RepeatCount <n>` (default 1).
  Formal Slice C commands use `-VerifyEvidence -RepeatCount 2`, so the same selected main or
  satellite run captures fresh team/watch/control cursors, runs, verifies evidence, then repeats
  once more without relying on the operator to remember a second command. `RepeatCount < 1` is
  rejected. Verified with `phase2-fleet.ps1 -SelfTest`.
- 2026-06-25 — **Phase 2 fleet satellite crews now preserve the 2-approval governance invariant.**
  `scripts/phase2-fleet.ps1` no longer generates satellite crews with `min_approvals = 1`, and the legacy root `crew-sat-two-ai.toml` sample was aligned too.
  Satellites still use the minimal codex+claude CLI set requested by the operator, but generated
  crews now run `codex` implementer + `claude` review + `codex` audit, so `min_approvals = 2`
  has two distinct reviewer vendors available. The machine-readable plan now exposes
  `min_approvals` and `reviewer_agents`, and `scripts/phase2-verify.ps1` rejects generated
  fleet plans that cannot satisfy the Phase 2 quorum. Verified with `phase2-fleet.ps1
  -SelfTest`, `phase2-goal-shape.ps1 -SelfTest`, `phase2-goal-shape.ps1 -Manifest
  examples\phase2-fleet.sample.json`, and `phase2-verify.ps1 -SkipSliceA -SkipSliceB
  -SkipSliceD -FleetManifest examples\phase2-fleet.sample.json -FleetNode m1`.
- 2026-06-25 — **Phase 2 fleet RunSelected can now verify run evidence automatically.**
  `scripts/phase2-fleet.ps1 -RunSelected -VerifyEvidence` now captures team/watch/control cursors
  before each selected manifest run, derives the run terminal (`landed` or `escalated`) from the
  actual `ensemble run` output, and invokes `scripts/phase2-run-evidence.ps1 -ExpectTerminal ...`
  afterward. Nonzero escalated runs still fail unless the operator explicitly adds
  `-AllowEscalatedRun`. This removes a manual Slice C error source: operators no longer need to
  hand-count cursors before using the manifest-driven fleet runner. Added
  `-RequireControlEvidence`, `-RequireSteerEvidence`, and `-RequireAbortEvidence` for runs where
  intervention happened.
  Verified with `phase2-fleet.ps1 -SelfTest`, `phase2-fleet.ps1 -Manifest
  examples\phase2-fleet.sample.json -Node all -PlanOnly -Json`, `phase2-goal-shape.ps1
  -Manifest examples\phase2-fleet.sample.json`, and `phase2-verify.ps1 -SkipSliceA
  -SkipSliceB -SkipSliceD -FleetManifest examples\phase2-fleet.sample.json -FleetNode m1`.
- 2026-06-25 — **Phase 2 smoke now verifies real run team/watch evidence.**
  `scripts/smoke.ps1` now runs the governed smoke with `--team <team>`, captures pre-run
  team/watch/control cursors, and invokes `scripts/phase2-run-evidence.ps1` after the run. This closes
  a real gap found by replaying the Slice D smoke repo: the old smoke had a watch terminal decision but
  no team-board run transcript because `ensemble run` was missing `--team`. Verified with
  `scripts\smoke.ps1 -NoBuild -TargetDir D:\tmp\ensemble-phase2-local-ready-target -SmokeRoot
  D:\tmp\ensemble-smoke-team-evidence -TimeoutSecs 180 -AgyTimeoutSecs 1`, then the full clean reinstall
  path `scripts\phase2-local-ready.ps1 -SkipPhase1 -SkipPhantom -SkipPhase2 -RunCleanReinstall` using
  `D:\tmp\ensemble-phase2-local-ready-clean2`, including uninstall -> install -> service dry-run ->
  smoke -> up/mesh/nodes -> final uninstall.
- 2026-06-25 — **Phase 2 per-run evidence verifier added.**
  `scripts/phase2-run-evidence.ps1` verifies the evidence left by one completed main or satellite run:
  callers must pass independent `-TeamSince` and `-WatchSince` cursors; `team inbox` must contain a
  conductor terminal decision, `watch --json` must expose the same terminal decision, and optional
  `-RequireControl` / `-RequireSteer` / `-RequireAbort` checks the run's control feed from
  `-ControlSince`. It is intended to run after each real m1~m5 Slice C run so completion is based on
  recorded team/stream/control evidence, not only terminal memory. Verified with a self-test temporary
  repo, a negative terminal-expectation check, a team/watch disagreement check, malformed control-feed
  rejection, invalid control-command shape rejection, terminal-prefix rejection (`ESCALATED_PENDING` is
  not terminal), and terminal-case rejection (`landed` is not `LANDED`).
- 2026-06-25 — **Phase 2 local readiness wrapper added.**
  `scripts/phase2-local-ready.ps1` chains the existing verifiers needed before moving to the real 5-node
  fleet: Phase 1 deterministic acceptance, Phantom single-machine bridge, and Phase 2 Slice A +
  Slice B governance preflight + Slice C local mesh/nodes. Clean reinstall remains opt-in via
  `-RunCleanReinstall` because it mutates the user-level install. Verified with the full default local-ready
  path, a faster `-SkipPhase1 -SkipPhantom` Phase 2-only path, and an all-skip negative check that must fail
  instead of fake-green.
- 2026-06-25 — **Phantom single-machine bridge verifier added.**
  `scripts/phantom-single-machine.ps1` checks the bridge between Phase 1 manual local testing and Phase 2
  fleet testing: direct `ensemble agent <agent> PONG --node local --json` must return `ok:true` and
  `node:"local"`, then `phantom tool shell` must invoke the same ensemble binary through a temporary
  `.cmd` shim and return the same local-node JSON shape. The script deliberately fails on non-shell-safe
  repo/prompt/shim arguments because the current Phantom shell bridge is sensitive to nested quotes. Verified
  on this host with the release binary from `D:\tmp\ensemble-acceptance-phase1-target`.
- 2026-06-25 — **`ensemble agent --node local` now forces the local adapter.**
  This closes an integration trap found while checking Phantom Mesh single-machine invocation:
  `ensemble agent codex ... --node local` previously tried `http://local:7878` and flaked instead
  of bypassing remote routing. The fix keeps bare hosts such as `ayaneo` routed remotely, while
  exact lowercase `local` is the explicit local escape hatch. Verified with the new bin regression test, full
  `cargo test --bin ensemble`, `cargo test --test cross_machine`, `cargo fmt --check`, deterministic
  single-machine acceptance, a live release-binary `agent codex PONG --node local --json` call, a
  Phantom `tool shell` call into that release binary, and Phase 2 Slice A verifier.
- 2026-06-25 — **Phase 2 Slice C now validates the generated full-fleet run/watch plan.**
  `scripts/phase2-fleet.ps1 -PlanOnly -Json` emits a machine-readable plan with nodes,
  projects, run/watch/service commands, team names, watch names, repos, and crew paths. Slice C
  in `scripts/phase2-verify.ps1` now consumes that JSON for `-FleetManifest` and verifies the
  manifest generates exactly 5 nodes, 1 main project, 4 satellites, 5 service/up commands, 5 run
  commands, and 5 watch commands, with each project matched to its generated run/watch command.
  Verified with `phase2-fleet.ps1 -SelfTest`, `phase2-fleet.ps1 -Manifest
  examples\phase2-fleet.sample.json -Node all -PlanOnly -Json`, `phase2-goal-shape.ps1
  -Manifest examples\phase2-fleet.sample.json`, and `phase2-verify.ps1 -SkipSliceA
  -SkipSliceB -SkipSliceD -FleetManifest examples\phase2-fleet.sample.json -FleetNode m1`.
- 2026-06-25 — **Phase 2 Slice A loopback now verifies remote abort.**
  `scripts/phase2-verify.ps1` already covered remote `team status/say/inbox`, `watch`, `steer`,
  explicit `--node auto` failures, and wrong-token mutation failures. It now also sends
  `ensemble abort <name> --hard --node <loopback-serve>` first with a wrong token (must fail
  `Unauthorized` and leave no abort record), then with the ephemeral token, and asserts the remote
  control feed contains exactly one hard-abort command. Verified with
  `pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -TargetDir
  D:\tmp\ensemble-phase2-baseline-target -SkipSliceB -SkipSliceC -SkipSliceD`.
- 2026-06-25 — **Phase 2 Slice C now has a manifest goal-shape preflight.**
  Added `scripts/phase2-goal-shape.ps1` to validate the real fleet manifest before operators
  touch the five machines: exactly 5 nodes, conductor included, main codex/claude/agy routes
  pointing at fleet nodes, exactly 4 satellites, and unique satellite name/team/watch values.
  This catches the most likely no-manual-routing mistakes before `phase2-fleet.ps1 -Materialize`.
  Verified with `phase2-goal-shape.ps1 -SelfTest`, `phase2-goal-shape.ps1 -Manifest
  examples/phase2-fleet.sample.json`, and the existing `phase2-fleet.ps1 -SelfTest`.
  Real m1~m5 `ensemble up` + main/satellite runs remain the next operator/fleet step.
- 2026-06-25 — **Phase 2 single-machine baseline and clean reinstall passed on Windows.**
  A current release `ensemble.exe` passed `scripts/acceptance-single-machine.ps1 -NoBuild`,
  covering team status/say/inbox, watch/steer/abort, controlled codex/agy launch paths,
  bounded agy visibility, and MCP team/control tools. The governed local smoke
  `scripts/smoke.ps1 -NoBuild -TimeoutSecs 240 -AgyTimeoutSecs 5` drove real
  `codex -> test gate -> claude -> LANDED -> merge`, with `supervise` returning `on_track`.
  Slice D also passed through `scripts/phase2-verify.ps1 -SkipSliceA -SkipSliceB -SkipSliceC`:
  uninstall baseline, install, service install/uninstall dry-run, smoke, `up`, `mesh`, `nodes`,
  and final uninstall. Post-run checks confirmed no installed `ensemble.exe` and no User PATH
  ensemble entry remained. This proves the local baseline and clean reinstall path on this host;
  it still does not prove the real m1~m5 Slice B/C fleet run.
- 2026-06-25 — **Phase 2 Slice B now has hermetic remote-governance coverage.**
  `tests/cross_machine.rs` starts in-process `ensemble serve` nodes and drives the conductor
  through `RemoteAdapter` for the HTTP/git-sync path. Positive coverage proves a remote codex
  implementer lands only after a real test-gate pass and two distinct remote reviewer verdicts
  (`claude` + `agy`). Negative coverage proves a red test gate escalates before reviewers run,
  and two reviewer roles backed by the same vendor do not satisfy `min_approvals = 2`. This is
  still not the real 5-node Slice B/C/D acceptance; those remain operator/fleet-run work.
- 2026-06-25 — **Phase 2 Slice B now preflights governance before a run.**
  New `ensemble crew inspect [--crew <path>] [--json]` uses the Rust TOML parser to expose the
  pipeline, min approvals, test command, reviewer agents, distinct reviewer count, and explicit
  active-role remote-agent routes. `scripts/phase2-verify.ps1` now calls it at the start of Slice B and refuses
  a crew without `[test]`, `min_approvals >= 2`, and at least two distinct reviewer vendors. It also
  has `-SliceBPreflightOnly` for deterministic gating checks and `-RequireExplicitRemoteAgents` for
  the real cross-machine acceptance pass. Added `examples/crew-phase2.toml` as the local fallback
  instead of the older Phase-1 sample, so Phase 2 cannot accidentally pass on an ungated crew.
- 2026-06-25 — **Phase 2 verifier now does a loopback remote-control smoke for Slice A.**
  `scripts/phase2-verify.ps1` starts a temporary loopback `ensemble serve` with an ephemeral port/token
  during Slice A, then drives the real CLI through `--node http://127.0.0.1:<port>`.
  It verifies remote `team status`, token-protected `team say`, token-free `team inbox`, remote `watch`,
  and token-protected `steer`; it also checks `team --node local` and explicit `team/watch --node auto`
  failure paths. This does not prove the real m1~m5 fleet is up, but it upgrades the cross-machine
  control-plane evidence from unit tests to a repeatable end-to-end CLI/HTTP smoke on one machine.
- 2026-06-25 — **Phase 2 verifier now understands the fleet manifest path.**
  `scripts/phase2-verify.ps1` accepts `-FleetManifest`, `-FleetNode`, and
  `-CheckFleetManifestNodes` for Slice C. With a manifest it now prints the same selected-node
  `phase2-fleet.ps1 -PlanOnly` output during verification, and with `-CheckFleetManifestNodes`
  it derives expected peers from `manifest.nodes` while skipping the current/local node. Verified
  with a sample manifest positive plan run, a negative missing-peer check on the current no-peer
  mesh, and a fake `ensemble mesh` positive check for m2~m5. This reduces the real 5-node operator
  loop to one manifest-driven verifier command on m1 after the nodes are serving.
- 2026-06-25 — **Phase 2 fleet service bootstrap moved under the manifest runner.**
  `scripts/phase2-fleet.ps1` now accepts `-Service install-print|install|uninstall-print|uninstall|up`
  and `-RunService`, so each host can use the same untracked `phase2-fleet.local.json` plus its
  explicit `-Node <host>` to preview or run its own `ensemble serve --install-service` lifecycle.
  `-RunService` refuses the default `all` node and refuses an omitted `-Service`, avoiding accidental
  foreground `up` or repeated local service mutations. Self-test now covers both selected run execution
  and selected service execution through a fake `ensemble`, and the Phase 2 goal/verify/SOP docs use
  the manifest-driven service bootstrap commands.
- 2026-06-25 — **Phase 2 local regression + clean reinstall slice reverified on Windows.**
  Current branch `phase2-verify-fixes` passed deterministic single-machine acceptance and the
  local Phase 2 verifier slice: `pwsh -NoProfile -File scripts\acceptance-single-machine.ps1
  -SmokeRoot D:\tmp\ensemble-acceptance-phase2-regression -TargetDir
  D:\tmp\ensemble-phase2-regression-target -AgyTimeoutSecs 1` passed, including team board,
  controlled codex/claude/opencode config previews, controlled agy launch, visible agy
  bounded-turn `flake`, MCP team/control tools, watch, steer, and abort. Then
  `pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -TargetDir
  D:\tmp\ensemble-phase2-regression-target -SmokeRoot D:\tmp\ensemble-phase2-clean-smoke
  -SkipSliceB -SkipSliceC -UpBind 127.0.0.1:0 -SmokeTimeoutSecs 180` passed Slice A and
  Slice D: clean uninstall/install, service install/uninstall dry-run, `ensemble up`,
  `mesh`, `nodes`, and a real codex→claude governed smoke that LANDED with supervisor
  `on_track`. This proves the local baseline and rebuildability path after tick C; the
  remaining Phase 2 evidence is the real 5-node Slice B/C/D run on m1~m5.
- 2026-06-25 — **OSS onboarding tick C service install implemented on `phase2-verify-fixes`.**
  `ensemble serve --install-service|--uninstall-service` now supports Windows Task Scheduler
  (`schtasks` logon task), macOS launchd user agents, and Linux systemd user units. Install/update
  starts or restarts the service now; uninstall stops it before removing persistent state. `--print`
  dry-runs the exact OS plan without mutating the machine, so Phase 2 Slice D can verify the path
  during clean reinstall. The service command defaults to plain `ensemble serve`, preserving the
  safe tailnet/loopback bind behavior; explicit `--bind` is persisted only when passed. Explicit
  `--token` can be persisted for mutation auth, but ambient `ENSEMBLE_TOKEN` is intentionally not
  baked into service files. Verification: red/green pure generator tests, CLI config tests,
  `cargo test service`, `cargo check`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
  `git diff --check`, and Windows `serve --install-service --print` / `--uninstall-service --print`
  dry-runs. NEXT: real 5-node Slice C/D service smoke on operator machines, then distribution tick A.
- 2026-06-23 — **Phase 2 CLI control routing + member routing + mutation token boundary implemented locally (pending double-gate/commit).**
  Operator-facing `team status|say|inbox`, `watch`, `steer`, and `abort` now accept explicit
  `--node <host|url>` and route through `RemoteControlPlane`; local default remains unchanged.
  Bare hosts normalize to `http://<host>:7878`, full URLs are used as base URLs, and `--node auto`
  is rejected until discovery/member routing exists. Added parser coverage, URL normalization tests,
  and a fake `/control` HTTP test proving CLI helper remote append-control posts the wire contract.
  Member-targeted live-control commands also accept `member@node` without `--node`; `--node`
  remains highest precedence, the full member name is preserved for the remote feed, and `@local`
  plus this machine's short hostname stay local to protect Phase-1 member names such as `codex@z13`.
  `member@node` now prefers a matching `discover_mesh` serve URL before falling back to
  `http://<node>:7878`, so MagicDNS/tailnet-IP fallback stays centralized in discovery.
  Remote mutation routes now have an optional shared-secret boundary: configured servers require
  `x-ensemble-token` for `team_say` and `append_control`, while read-only `team_status`,
  `team_inbox`, and `watch` stay readable. Server token source: `ENSEMBLE_TOKEN` or
  `serve|up --token`; client token source: `--token` or `ENSEMBLE_TOKEN`; blank/control-character
  tokens are ignored per source and never logged, so an invalid explicit `--token` does not suppress
  a valid `ENSEMBLE_TOKEN`. `--node local` forces the local file-backed plane as an escape hatch for
  existing local member names like `reviewer@work`; explicit loopback nodes such as `localhost` and
  `127.0.0.1` still route through HTTP so the same remote-token boundary can be tested locally.
  Verification on Windows target dirs
  `D:\tmp\ensemble-target-phase2-cli-node-final`, `D:\tmp\ensemble-target-phase2-auth`, and
  `D:\tmp\ensemble-target-phase2-member-node-final`, and
  `D:\tmp\ensemble-target-phase2-discovery-final`: focused remote/helper/auth/member-routing tests
  plus Phase-1 gates `cargo test control`, `cargo test team_`, and `cargo test launcher` passed
  after the routing slice. NEXT: coherent multi-node team state.
- 2026-06-22 — **per-agent config (item 6 / 設定) LANDED @a95e2d6** (codex+claude LGTM **FIRST round** —
  the 4th and final verb of the single-machine 主控台). crew.toml `[agents.<n>] args = [...]` (extra flags
  appended to the LOCAL invocation; `["--model","x"]` selects a model) + `timeout = N` (per-command seconds).
  `AgentConfig` +`args`/`timeout` (serde-default, old configs unaffected) + `args_for`/`timeout_for`;
  `ExecAdapter::with_extra_args`; `adapters_for` applies via `cfg_exec`/`cfg_agy` (LOCAL only — remote nodes
  configure themselves). TDD: parse test + `with_extra_args` test; smoke: `[agents.codex] timeout = 2` flaked
  codex in ~2s. **MILESTONE: the single-machine supervision QUARTET is complete on both platforms —
  observe (S1a `--watch`/`watch`) + interrupt (S1b `abort [--hard]`) + adjust (S1b `steer`) + configure
  (item 6) — all file-plane, zero ConPTY.** 215 lib tests; clippy clean. NEXT for 0.5: cross-machine control
  (serve `/stream`+`/control` routes; S2 ApiChannel / S3 PtyProxy). The operator's Stage-1 office test
  (Win + Mac single-machine multi-CLI + observe/interrupt/adjust/configure) is now fully buildable — test
  commands in `docs/2026-06-21-fleet-federation-night-runbook.md`.
- 2026-06-22 — **live-supervision S1b LANDED @cfc1760 (中斷+調整 / control plane)** (codex+claude LGTM
  **FIRST round**, no re-gate — item 0.5; plan `docs/plans/2026-06-22-s1b-supervision-control-plane.md`).
  The operator can now keep a drifting/wedged AI in line on a live `ensemble run --watch <name>`:
  **`ensemble steer <name> "<prompt>"`** injects a redirect into the NEXT round, **`ensemble abort <name>
  [--hard]`** stops the run (clean = next round boundary; `--hard` = kill the running CLI MID-TURN).
  Transport: a per-run control feed `.ensemble/control/<name>.ndjson` (reuses `ndjson::Feed`). New:
  `ControlCmd` (`cmd`-tagged) + `ControlState`/`drain_control` (supervise.rs); the conductor drains steers
  into `feedback` and streams `operator/steer`, `aborted()` now also reads the control state and streams
  `operator/interrupted`, and a `run_adapter` helper arms each adapter with `ctrl.hard_flag()`; the `Adapter`
  trait gains `set_abort` (default no-op) which `ExecAdapter`/`AgyAdapter` honor by killing the child in
  their exit-poll loop; a daemon watcher (cursor from feed END) feeds the `ControlState`. Clean-vs-hard is
  cleanly separated (adapters watch the HARD flag only, so a clean abort never kills mid-turn). **TDD** on
  the lib (ControlCmd roundtrip + path confinement; drain_control; conductor steer-injection + control-abort;
  exec_adapter mid-turn-kill < 3s); IO shell (steer/abort cmds + watcher) gate-reviewed + smoke-tested.
  **Smoke (z13):** `steer`/`abort` wrote the control feed; `abort --hard` during a live codex turn KILLED it
  mid-essay (`implementer failed: codex aborted by operator`) — the run stopped fast instead of writing the
  essay. 213 lib tests; clippy `--all-targets` clean. **Honest limit (intended):** steer is next-round, a
  clean abort is boundary-only, only `--hard` is mid-turn; true mid-turn INJECT is later S2/S3 (M-API/M-PTY).
  So the **observe + interrupt + adjust** triad of the 主控台 is now live on both platforms (file-plane, zero
  ConPTY). NEXT: the **設定 / configure** verb (item 6 — per-agent model/args/timeout). Runbook for the office
  Stage-1 test: `docs/2026-06-21-fleet-federation-night-runbook.md`.
- 2026-06-21 — **live-supervision S1a LANDED @ad7a6d8 (看得到 / observe)** (codex+claude LGTM after **1
  re-gate round** — item 0.5; plan `docs/plans/2026-06-21-s1a-conductor-live-stream.md`). Makes a governed
  `ensemble run` watchable IN REAL TIME: a `RunObserver` trait + production `FeedObserver` (append each
  blackboard `Message` to the S0 `ndjson::Feed`, best-effort — a write failure is swallowed so it can NEVER
  change a run) is injected via `ensemble run --watch <name>`; a single `note()` funnel in the conductor
  replaces every `bb.post` in `run()` and ALSO streams a terminal `decision` (LANDED/escalated/max-rounds);
  `ensemble watch` renders BOTH member-session `StreamEvent`s and run `Message`s. TDD on the lib (render
  Message, conductor mirrors incl. terminal decision, FeedObserver appends); the `--watch` IO shell is
  gate-reviewed + smoke-tested. **The double-gate earned its keep:** codex CHANGES r1 caught a real
  forward-compat bug claude had RATIONALIZED as non-blocking — an unknown future `"ev"`-tagged line fell
  through to `Message` render (serde ignores the extra field) instead of showing raw; fixed to render as a
  `Message` ONLY when there is no `"ev"` key (gate on key presence, not field shape) + a regression test;
  re-gate codex+claude both LGTM. Smoke (z13): `ensemble run --watch s1demo` → `ensemble watch s1demo` shows
  `[codex · result]` / `[agy · verdict]` / `[conductor · decision]` live. 207 lib tests; clippy clean (WSL).
  **Reliability finding → [[fleet-cli-reliability-governed-runs]]:** the smoke run escalated because **agy as a
  local reviewer can't reliably SEE the worktree files** under ConPTY (it referenced a phantom `diff.txt` and
  falsely rejected) — keep agy out of being the SOLE file-existence reviewer; pair it with codex/claude.
  **Operator runbook for the fleet federation written:** `docs/2026-06-21-fleet-federation-night-runbook.md`
  (bring-up steps + the per-agent-name node-routing constraint + all the day's gotchas). NEXT: **S1b** control
  plane (steer/abort), OR the operator's 5-machine federation (acer/Mac bring-up) → phantom-mesh.
- 2026-06-21 — **Cross-machine EDIT-RETURN proven ✅ (remote implementer).** Completes the federation-path set:
  a run with **codex@AYANEO as the IMPLEMENTER** (`[agents.codex] node = ayaneo` + `--no-discover`; claude/agy
  the local z13 gate) created SYNC.txt ON ayaneo → the edit **bundled back to z13 via repo_sync** → claude@z13
  + agy@z13 both LGTM → LANDED → merged onto z13's `main` (2m30s, SYNC.txt = "edit returned from ayaneo"). So
  ALL FOUR federation paths now hold: remote agent execution, remote reviewer in the gate, cross-machine
  auto-merge, and remote edit-return. **Discovered follow-up (minor):** a remote-bundled commit lands with a
  generic subject (`ensemble: codex-0`) instead of the task text the LOCAL implementer path uses — repo_sync's
  remote-bundle commit naming should carry the task description for parity. NEXT: cross-PLATFORM leg (a Mac
  peer) → full 5-machine federation → phantom-mesh.
- 2026-06-21 — **Phase-1 step 4 CROSS-MACHINE governed proof ✅ (z13 + ayaneo over tailnet).** First real
  federated `ensemble run`: **codex@z13** implemented FLEET.txt → gate = **claude@z13 + agy@AYANEO** (one
  reviewer executing on a DIFFERENT machine via `[agents.agy] node = "http://ayaneo…:7878"`, `--no-discover`
  so codex/claude stay local while the explicit node routes agy remote) → **2/2 LGTM → LANDED after 1 round →
  auto-merged into main** (2m47s, `--merge` clean now that `.ensemble/`+`crew.toml` are gitignored so the
  worktree stays clean). Bring-up: operator put Surfshark off / tailnet already up; z13 Taildropped today's
  release binary to ayaneo; ayaneo ran `ensemble serve` (binds 100.107.205.98:7878, all 4 CLIs [ok]). z13-side
  proven incrementally: `ensemble nodes` (1.6s, ayaneo offers all 4 agents) → `ensemble agent claude PONG
  --node ayaneo` (9s, ok) → `ensemble agent agy "…VERDICT: LGTM" --node ayaneo` (27s, validated the remote
  reviewer BEFORE the long run — the lesson from the opencode 23-min waste) → the cross-machine gated run. So
  the conductor's automated implement→cross-vendor-gate→land pipeline works ACROSS MACHINES, not just locally.
  Remaining for full step 4: add a Mac peer (cross-PLATFORM leg — M5 needs an on-Mac `cargo build --release`,
  or use the online dev-host) and a 4th vendor in the mesh. NEXT toward the operator goal: expand to the full
  5-machine federation → point the fleet at phantom-mesh. Plan: `docs/plans/2026-06-21-stage2-federation-and-oss-release.md`.
- 2026-06-21 — **Phase-1 step 4 LOCAL governed proof ✅ (z13, release binary).** First real automated
  `ensemble run` through the conductor: a trivial task (create GREETING.txt) → **codex** implemented it →
  **claude + agy** both returned `VERDICT: LGTM` → gate reached 2/2 quorum → **LANDED after 1 round (1m43s)**;
  then `ensemble merge` landed the kept branch `ensemble/Create-a-file-…-0` onto `main` (the `--merge` auto-land
  first correctly REFUSED a dirty worktree — untracked `crew.toml`/`.ensemble/` — so a `.gitignore` + explicit
  `ensemble merge` completed it). Proves the AUTOMATED implement→cross-vendor-double-gate→land pipeline works
  end-to-end with real CLIs, not just the manual Bash-orchestrated gate. **Also proven earlier this session:**
  `ensemble agent claude` single-turn (PONG, ~16s, ok), `ensemble doctor`/`mesh` all-green on z13 (4 CLIs +
  tailscale + git-repo). **Reliability finding → [[fleet-cli-reliability-governed-runs]]:** codex/claude =
  reliable verdict-emitters; agy usable as reviewer (~13s, clean marker, ConPTY); **opencode HANGS headless
  (timed out 600s × 2 = ~23min wasted as review2) — exclude from headless governed roles** until item 6's
  per-agent timeout knob. Lesson recorded: validate a non-core agent (`ensemble agent <cli> "…VERDICT: LGTM"`)
  BEFORE a long governed run. NEXT: the CROSS-MACHINE 4-CLI proof — operator brings up Tailscale (Surfshark off
  → `tailscale up`), workers `ensemble serve`, then smallest-proof (z13 + 1 Mac, 2 vendors) → full 5-machine
  federation → point the fleet at phantom-mesh. Plan: `docs/plans/2026-06-21-stage2-federation-and-oss-release.md`.
- 2026-06-21 — **live-supervision S0 LANDED @7f1ded4** (codex+claude LGTM after **1 re-gate round** — item 0.5,
  operator-flagged "必要"; spec `docs/specs/2026-06-21-live-supervision-design.md`, plan
  `docs/plans/2026-06-21-s0-supervision-file-plane.md`). The read-only STREAM file-plane: **`src/ndjson.rs`** a
  generic append-only NDJSON `Feed` (generalizes board.rs's fs2 locking — exclusive append / shared read /
  torn-tail repair / per-line JSON validity; `append` returns the cursor read back UNDER the still-held lock so
  a poller never skips a concurrent append) + **`src/supervise.rs`** `StreamEvent` (serde internally-tagged on
  `"ev"`; an unknown future kind or torn line renders RAW via `render_line`'s `? {raw}` fallback — never hidden)
  + `member_stream_path` (untrusted member name → ONE filename component under `<repo>/.ensemble/stream/` via the
  shared `journal::sanitize_slug`, now `pub(crate)`) + `parse_watch_args`; the IO shell **`ensemble watch
  <member> [--repo][--since][--follow]`** tails + renders the feed (250ms poll on `--follow`). **TDD on the lib
  (204 tests), gate-review+smoke on the IO shell** per convention. **The double-gate earned its keep — codex
  CHANGES r1, claude LGTM r1 (split → no land):** codex caught 2 real landing-blockers — (1) `Feed::append`
  accepted a valid-but-MULTI-LINE JSON value (`[\n1\n]`, a pretty object), which NDJSON splits on `\n` would
  fracture into a DIFFERENT record on read → now rejects any interior `\n`/`\r` after trimming the trailing
  terminator (+ regression test, claude had flagged the same as a non-blocking note); (2) `watch_cmd` ignored
  `writeln!`/`flush` errors and advanced the cursor anyway → a closed stdout pipe could spin `--follow` forever
  while dropping output → `drain` now propagates the io::Error and advances the cursor only AFTER a successful
  write; the caller classifies BrokenPipe (or Windows `ERROR_NO_DATA` raw os 232) as a clean consumer-gone exit 0,
  any other error exit 1 (matches `tail -f`: a gone consumer is detected on the NEXT write). **Re-gate: codex +
  claude both LGTM.** Smoke (Windows native): static/`--since` render correctly; `--follow` exits 0 when a new
  event arrives after the downstream `head` closes the pipe. 204 lib tests + clippy `--all-targets` clean (WSL).
  NEXT: **S1** control plane (`.ensemble/control/<member>.ndjson` + `ensemble steer`/`abort` + serve
  `/stream`+`/control` + conductor stream wiring). Parallel: operator finishing Stage-2 (Tailscale 5-machine
  cross-platform federation) — playbook in `docs/plans/2026-06-21-stage2-federation-and-oss-release.md`.
- 2026-06-21 — **auto member-name default LANDED @a8e7d69** (codex+claude LGTM **first round** — item 0.6, operator-requested). `ensemble mcp install` now defaults `--name` to `<client>@<short-host>` (e.g. `claude@z13`) instead of bare `<client>`, so members across the fleet don't collide with zero coordination and the identity is STABLE across restarts (ledger claim-ownership depends on it — deterministic per (client, host), never a per-launch counter). Pure `default_member_name` (host first-label → `[a-z0-9_-]`, lowercase; degenerate host → bare client) is TDD'd (3 tests); IO `raw_hostname` reads `COMPUTERNAME`→`HOSTNAME`→`hostname` cmd (no shell, no injection). Explicit `--name` still wins. Both reviewers traced tameness (no board/SQL/arg corruption), stability (no counter/time/rand; FQDN-vs-short + case normalized), safe degradation (every odd host → bare, never `client@`/panic). 187 lib tests; clippy clean both platforms; empirical `--print` on host YOYOGOOD bakes `claude@yoyogood`. NEXT autonomous-buildable: awaiting operator go on **0.5 live supervision** (design delivered, was design-approval-gated). Operator bringing up Tailscale in parallel → then Phase-1 step 4 live 4-CLI proof.
- 2026-06-21 — **`ensemble mcp install` LANDED @9329d4c** (codex+claude LGTM after **8 gate rounds** — OSS-onboarding one-click MCP register). `ensemble mcp install --client <claude|codex|opencode> [--repo --name --exe --crew --config --print]` writes the chosen CLI's REAL MCP-server config so it launches `ensemble mcp` and becomes a crew member — no hand-editing per-client formats. claude → project `.mcp.json`; opencode → project `opencode.json` (per-call timeout 600000 so a long `ensemble_run` isn't killed); codex → user `~/.codex/config.toml` (`$CODEX_HOME` honored). Pure heart `render_merged` in NEW **`src/mcp_install.rs`** (toml_edit STRUCTURAL for codex — no text markers, idempotent, format/comment-preserving; serde_json for claude/opencode — preserves other servers, errors-not-clobbers on malformed/non-object); the IO shell `mcp_install_cmd` DERIVES exe(`current_exe`)/repo(cwd)/home/`$CODEX_HOME`, all `absolutize`d, and writes ATOMICALLY via the `tempfile` crate (random O_EXCL temp in the target dir → `persist` rename; no half-write/corruption). **The double-gate earned its keep — codex (empty temp cwd, adversarial) peeled back a real IO-shell issue in nearly every round, each fixed at root:** (r1–4) renderer rewritten text-markers→toml_edit; (r4) predictable-temp TOCTOU → `tempfile` crate; (r5) home `USERPROFILE`-before-`HOME` on EVERY OS / dangling config symlink destroyed (`resolve_replace_target` follows it, link preserved) / Windows perm-copy only the readonly-attr; (r6) `NamedTempFile` cleanup defeated by `process::exit` (could leave a `.ensemble-mcp-*` copy of the config in the dir) → temp lifecycle isolated in `write_config(...)->Result` that NEVER exits while the temp is live + Windows ACL invariant scoped HONESTLY (inherit the dir ACL, no security-crate DACL clone — these configs hold no secrets, only an exe path + args); (r7) `absolutize` returns a RELATIVE path if `current_dir()` fails → could bake `--repo .` / `./crew.toml` into codex config (its path is absolute via `$HOME`, so the location-guard missed it) → a guard now validates exe/repo/crew `is_absolute()` before rendering. 15 renderer unit tests + empirical Windows smoke (valid JSON, idempotent byte-identical, `--print` side-effect-free, malformed aborts without clobber, NO stray temp on success OR error). 184 lib tests + clippy clean on Windows + WSL. Runbook Part 1 updated to lead with `ensemble mcp install`. **NEXT: operator-requested LIVE-SUPERVISION items now queued (priority item 0.5 + auto-name, below) — operator flagged cross-session supervision "必要"; do them BEFORE OSS tick C/A.** Then Phase-1 step 4 live 4-CLI proof (Tailscale + cross-machine).
- 2026-06-21 — **ensemble mcp SLICE 4b-ii LANDED @d5f6208** (codex+claude LGTM after **2 gate rounds** — Phase-1 step 3 slice 4b-ii; **MCP API now CODE-COMPLETE, all 10 tools**). Adds **`ensemble_run {task}`** so a LIVE CLI crew member can delegate a WHOLE task to a headless governed crew sub-run (the full implementer → test-gate → reviewers → gate pipeline, in its own throwaway worktree; BLOCKS until terminal; on LAND the work is kept on a branch the member lands with `ensemble_merge`). **Design: a `CrewRunner` trait injected into `Ctx`** (`Option<Arc<dyn CrewRunner: Send+Sync>>`), NOT a `Conductor` built inside mcp.rs — so the library stays free of crew/adapter construction (binary-only wiring) AND the happy path is hermetic (a `FakeRunner` records task+repo, returns a canned `RunSummary`; a real minutes-long multi-CLI run is never spawned in a unit test). `main.rs` `ConductorRunner` wraps `Conductor::run_in_repo`; `mcp_runner` builds it ONCE at startup from `--crew`/`<repo>/crew.toml` (no tailnet PROBE at startup — but an explicit `node=` in crew.toml still resolves to a RemoteAdapter); a missing crew.toml leaves the runner `None` so the server STILL starts and the other 9 tools work. Runs in `ctx.repo`, never a client path; `task` required non-blank validated BEFORE the runner is touched. **Gate r1 (codex CHANGES, claude LGTM — split again):** tools/list advertised `ensemble_run` even when production startup can leave the runner unconfigured → tools/list "lied" about capability + the test locked in the bad invariant. **Fix:** `tools_list(ctx)` appends `ensemble_run` IFF `ctx.runner.is_some()` (the 9 crew-less tools always listed); test rewritten to pin BOTH directions; `tool_run`'s -32603 kept as defense-in-depth. **Gate r2: codex+claude both LGTM** (claude verified Send/Sync via `Adapter: Send+Sync` → `Conductor` Send+Sync, stdout-Mutex-not-held-across-run, branch-never-null-on-land via run_in_repo's only Landed path, partial-move soundness; its non-blocking note that "LOCAL adapters only" overstated isolation → doc tightened). 5 TDD mcp tests (RED-verified). NEXT: Phase-1 step 4 live 4-CLI proof (operator machine) → autonomous queue resumes at OSS onboarding tick C.
- 2026-06-21 — **ensemble mcp SLICE 4b-i LANDED @e6e25a8** (codex+claude LGTM **first round** — Phase-1 step 3 slice 4b-i). Closes the gap codex flagged in 3b: a task claimed over MCP had NO path to a terminal state (it stayed `claimed` until `recover_orphans` requeued it). Adds **`ensemble_complete {id, outcome}`** + **`ensemble_fail {id, reason}`** so the CLAIMING member can close it out. New `ledger::complete_owned`/`fail_owned`: a SINGLE atomic guarded `UPDATE tasks SET state=… WHERE id=? AND state='claimed' AND claimed_by=?` — the ownership check + terminal write are one SQLite statement (no check-then-act race), returns true iff the transition happened. The `state='claimed'` predicate is load-bearing: it also defends the orphan/requeue case (if a stale claim is requeued without clearing `claimed_by`, the prior owner's late complete is correctly rejected). Worker = `ctx.name` (server identity, never a client field → no completing-as-another); a blocked write is a reported OUTCOME `{completed:false, id, detail}` (like a merge conflict), NOT a protocol error. The unguarded `complete`/`fail` stay for the headless `dispatch` driver (which owns its whole queue). 3 ledger + 5 mcp TDD tests (RED-verified): ownership rejection leaves state untouched, no row created for an unknown id, double-complete doesn't overwrite, -32602 field validation. Gate non-blocking notes → backlog deferrals: (a) close-out isn't idempotent on retry (a re-issued complete after success → `{completed:false}` already-terminal); (b) a symmetric double-fail test for parity. NEXT MCP slice: ④b-ii `ensemble_run`.
- 2026-06-21 — **ensemble mcp SLICE 4a LANDED @0121465** (codex+claude LGTM after **4 gate rounds** — Phase-1 step 3 slice 4a). Adds **`ensemble_merge {branch, into?}`**: a live member LANDS its kept-worktree branch (`ensemble/<member>/<task>`) onto a target (default "main") via the gated `repo_sync::merge_branch` — fast-forward or true-merge; CONFLICT aborts+restores (decision 2) and is returned as a reported OUTCOME `{landed:false, conflict:[paths]}`, not an error. Concurrency: merge mutates the MAIN worktree (checkout into + merge), so concurrent merges (MCP request threads) are serialized by a per-repo exclusive fs2 lock anchored to the COMMON git dir (`worktree::git_common_dir` made pub(crate); same anchor proven in 3a). **The double-gate earned its keep again:** claude LGTM'd all 4 rounds; codex peeled back git ref-confusion layer by layer — r1 path-checkout (`into:"f"` where f is a tracked file → `git checkout f` lands on the WRONG branch, reports success), r2 leading-dash flag-injection (`refs/heads/--detach` creatable via plumbing → parsed as a git flag), r3 special-ref (`into:"HEAD"` resolves the pseudoref not a branch named HEAD; same-named tag shadows; `refs/heads/main`-looking names). Final guard: reject leading `-`, THEN require `git rev-parse --quiet --verify --symbolic-full-name <name>` == `refs/heads/<name>` (proves the raw short name resolves, under git's OWN rules, to exactly that local branch — also kills tag-shadowing). Also a client-controlled-arg → git-flag-injection guard. 9 mcp tests (TDD, RED-verified) incl. concurrent-merges-both-land + each ref-confusion case. NEXT MCP slice: ④b `ensemble_run` + `ensemble_complete`/`ensemble_fail`. Then step 4 live 4-CLI proof.
- 2026-06-21 — **ensemble mcp SLICE 3b LANDED @3ecf1c0** (codex+claude LGTM **first round** — Phase-1 step 3 slice 3b). Adds the durable WORK-QUEUE half of the crew API over MCP: **`ensemble_enqueue {descr}`** (id = `dispatch::task_id(descr)` stable hash → idempotent; returns `{enqueued,id}`) + **`ensemble_claim`** (no args; atomically claim the OLDEST queued task as THIS member `ctx.name`, AT-MOST-ONCE; returns `{claimed:true,id,descr}` or `{claimed:false}`). Backed by the already-gated SQLite `ledger::Ledger` at the per-repo `<repo>/.ensemble/ledger.db` (mirrors the board). **No extra lock needed** — the ledger's IMMEDIATE transaction already serializes claims across connections/processes (proved by `ledger::claim_is_at_most_once_under_concurrency`), and each MCP call opens its own connection. `open_ledger` create_dir_all's `.ensemble/` (SQLite won't make the parent). Identity is server-enforced (`ctx.name`, not a client field → no claiming-as-another); `descr` is a bound SQL param. 6 TDD tests (RED-verified). Gates' non-blocking notes → backlog deferrals: (a) `task_id` DefaultHasher isn't stable across toolchain upgrades — fine for dispatch's transient queue but the durable ledger could silently dup post-upgrade → content-stable hash someday; (b) complete/fail/recover not yet over MCP → pairs with slice 4. NEXT MCP slice: ④ `ensemble_merge`+`ensemble_run` (+ `ensemble_complete`/`ensemble_fail`). Then step 4 live 4-CLI proof.
- 2026-06-21 — **ensemble mcp SLICE 3a LANDED @f5f82fa** (codex+claude LGTM after **8 gate rounds** — Phase-1 step 3 slice 3a). Adds the **`ensemble_worktree {task?}`** tool so a LIVE CLI crew member gets an ISOLATED, PERSISTENT git worktree to work in (then lands it via `ensemble merge`; slice 4 will expose merge over MCP). New `worktree::KeptWorktree {path,branch,slug}` (a plain handle, NO Drop — unlike the conductor's RAII `Worktree` which removes the dir on drop) + `worktree::ensure_kept_worktree(repo, member, task)`. The double-gate worked HARD here: codex (adversarial) found a real correctness bug in nearly every round, each fixed at root — (r1) slug `<member>-<task>` was ambiguous since sanitize emits `-` → member/task are now SEPARATE path components `.ensemble/worktrees/<member>/<task>`; (r1) TOCTOU under the MCP server's per-request threads → creation serialized by a per-repo exclusive fs2 lock; (r2) suffix-match false-positive → EXACT path match; (r2) stale/prunable re-attach returned a phantom path + reconstructed branch → prunable/missing ERRORS, re-attach reports git's ACTUAL branch; (r2) `task:null` → defaults to "work" (consistent w/ board_read's `since`); (r3) DETACHED worktree (no porcelain `branch` line) → errors instead of fabricating the canonical branch; (r4) prune leaves the branch behind so recreate failed `-b` → create path now REUSES a surviving branch (`git worktree add <path> <branch>`), restoring committed work; (r5) lock keyed on ctx.repo not the repo's COMMON git dir → anchored to `git rev-parse --git-common-dir` so all worktrees of a repo serialize on one lock; (r6→r7) a canonicalize() detour to harden path-matching was REVERTED because it resolves symlinks (a symlink squatting the target would match main) → settled on LEXICAL component-wise `Path` equality (separator-safe in Rust, symlink-safe, casing consistent from git output); symlink squatter now caught as unregistered. 10 worktree + 6 mcp tests (TDD, each round RED-verified). ⚠️ LESSON: the final amend used `git add -A` which swept a stray `target-review/` build dir (3309 files) into the commit + main (unpushed → fixed via `git rm -r --cached` + amend; `.gitignore` now has `/target-*`). Use `git add <specific files>` — saved to memory. NEXT MCP slice: ③b `ensemble_claim` (ledger at-most-once) → ④ `ensemble_merge`+`ensemble_run`. Then step 4 live 4-CLI proof.
- 2026-06-21 — **ensemble mcp SLICE 2 LANDED @3b5a96d** (codex+claude LGTM, 2 gate rounds — Phase-1 step 3 slice 2). Adds the **`ensemble_board_post {kind, body}`** write tool so a LIVE CLI crew member can post to the shared repo blackboard (it could already read via `ensemble_board_read`). `tool_board_post`: author is the server identity (`ctx.name`), NEVER a client field → no impersonation; `required_str` maps every bad-field mode (absent/null/non-string/blank-after-trim) to a precise `-32602` that names the field, checked BEFORE the post so a malformed call never writes a junk line; returns `{posted, next}`. **Gate r1 (codex CHANGES, claude LGTM — split):** both reviewers spotted the SAME phenomenon (cursor overshoot under concurrent posters) but disagreed on severity; the double-gate requires BOTH → did NOT land. The old code did `post()` then a SEPARATE `len()`; if member B interleaves a post between A's two calls, `len()` observes the later state and A's returned `next` SKIPS B's message forever. **Fix:** `FileBoard::post` now returns `io::Result<usize>` = the cursor read back via `count_messages` WHILE STILL HOLDING the exclusive append lock (same non-empty+parseable filter as `read_since`, so the cursor is consistent with a reader's index) → race-free & lossless. The 20-thread concurrency test now asserts the returned cursors are a gap-free permutation of `1..=20`. `kind` also bounded (MAX_KIND=64; it's fully client-controlled unlike the already-excerpted body) per codex's non-blocking note. **Gate r2: codex+claude both LGTM** (codex verified no deadlock/wrong-count/fd-seek bug; claude added a doc-only note, since added, that a count-readback failure after a successful append surfaces as Err though the message landed → at-least-once). Also cleared 4 pre-existing clippy `doc_lazy_continuation` warnings in repo_sync.rs (a wrapped doc line started with "+ " → CommonMark bullet; reworded). 9 new tests (7 mcp + 2 board) + strengthened concurrency test; full suite 125 lib + integration green; clippy clean. NEXT MCP slice: ③ `ensemble_claim`(ledger at-most-once) + `ensemble_worktree`(a KEPT worktree, not the RAII-drop one) → ④ `ensemble_merge`+`ensemble_run`. Then Phase-1 step 4 live 4-CLI proof.
- 2026-06-21 — **ensemble mcp SLICE 1 LANDED @ad3749f** (codex+claude LGTM; codex 4 rounds + claude 2 rounds, ~9 findings — Phase-1 step 3 slice 1; spec `docs/specs/2026-06-20-ensemble-mcp-design.md`). A LIVE CLI launching `ensemble mcp` becomes a crew member. Operator chose "async but as real-time as possible" → I analyzed: **hand-rolled minimal MCP over stdio, NO tokio** (newline-delimited JSON-RPC 2.0, each request on its own thread, stdout serialized by a Mutex) — ensemble's primitives are synchronous+blocking, so a thread is the natural concurrency unit (no spawn_blocking churn); a long tool call never blocks a concurrent quick one. Subset: initialize/notifications/tools.list/tools.call. New **`src/board.rs` FileBoard** — the persistent MULTI-PROCESS crew blackboard at `<repo>/.ensemble/board.jsonl` (session=repo), posts SERIALIZED by an OS file lock (added **fs2** dep): exclusive-append/shared-read → append order == file order == read order → positional `read_since(n)` cursor LOSSLESS under concurrent writers; torn-tail repair before append; per-line `from_slice` byte parsing (a bad/non-UTF-8 line skipped individually). Tools (read-only): `ensemble_mesh` (reuses discover_mesh+present_clis+render_mesh), `ensemble_board_read {since}`→{messages,next}. **Gate findings fixed:** atomic-append claim was wrong (→fs2 lock); count cursor lost out-of-order publishes (→serialized appends); torn-tail swallowed the next post (→repair); JSON-RPC conformance (-32700 parse error/id-null-is-a-request/-32600 bad method); `since` validation (-32602); unbounded request threads (→semaphore, MAX_INFLIGHT=16 + backpressure); **permit leaked on a handler panic → RAII PermitGuard** (16 panics would wedge serve). 18 hermetic tests (7 board, 11 mcp) + empirical stdio smoke (handshake/parse-error/board_read). NEXT MCP slices: ② `ensemble_board_post` (write tool over FileBoard.post) → ③ `ensemble_claim`(ledger)+`ensemble_worktree`(kept) → ④ `ensemble_merge`+`ensemble_run`. Then Phase-1 step 4 live 4-CLI proof (needs operator's machine).
- 2026-06-20 — **merge verb CLI GLUE LANDED @2890cd1** (codex+claude LGTM; Phase-1 step 2b CLI wiring — decision 2 now COMPLETE end-to-end). `ensemble merge <branch> --resolver <agent>`: on conflict run ONE AI-resolver round — resolves a LOCAL adapter (`resolve_one(&agent, None, false)`; a URL/unknown name hits the `_ => None` arm → exit 2 upfront) + calls the gated `merge_with_resolver` with a closure running `adapter.run(build_resolver_prompt(...), repo)` (edit-only prompt: remove markers, don't stage/commit). `ensemble run "<task>" --merge [--into <t>]`: auto-land the kept branch after a LANDED run; an auto-merge conflict is a SOFT failure (work safe on the branch, run still exit 0) reported with the `--resolver` retry hint. `--merge` added to BARE_SWITCHES. Gate r1 (codex): validate `run --into` (value-less `--into` no longer silently → main) + carry a non-default `--into <t>` into the retry hint. claude's lone caveat (explicit-URL `--resolver` → RemoteAdapter) was a MISREAD — it conflated resolve_one's `name` vs `explicit_node` params; merge_cmd passes `explicit_node=None`, so a URL name → exit 2 (comment accurate, no change). 2 unit tests + empirical CLI smoke (ff→0, conflict→3 tree-clean, bad-resolver→2). NEXT Phase-1: (3) `ensemble mcp` crew-participation API — researching rmcp + a design proposal for operator review before building (operator chose "glue first, then MCP design").
- 2026-06-20 — **merge_with_resolver MECHANISM LANDED @f2c3eb6** (codex+claude LGTM; 3 codex rounds / 7 findings + claude adversarial trace — two-phase spec Phase-1 STEP 2b / locked decision 2 CORE). On a git merge CONFLICT, run ONE AI-resolver round (a `resolve` closure given the repo + conflicting paths edits files in place — production runs a vendor CLI; repo_sync stays adapter-free so the dangerous git-safety logic is ONE place, hermetically testable via closures). COMPLETE the merge only if PROVABLY clean — no git conflict marker survives AND nothing left unmerged — else restore `into` to its exact pre-merge commit + `Conflict(paths)` to escalate. Heavily gate-hardened (lands onto main): (r1) `git add -A` masked markerless structural conflicts → only CONTENT conflicts go to the resolver, staged via `git add -- <named paths>`; UTF-8 marker scan → BYTE scan; `unmerged_paths` surfaces git errors; `restore_to` = single `reset --hard <pre_sha>` (clears MERGE_HEAD AND undoes a resolver commit) + `clean -fdq`, both error-checked. (r2) a self-committing resolver could land markers (commit then overwrite worktree clean) → resolver is EDIT-ONLY, not-mid-merge ⇒ restore+escalate (never trust a self-completed merge); marker-only structural classification could misfire on a marker-like line → classify by git index STAGES (`is_text_content_conflict`: stage 2 AND 3 present) AND markers, so binary/modify-delete/coincidence all escalate. claude confirmed both hard invariants (no marker/unmerged can land; every non-land path restores) under adversarial tracing. 7 hermetic tests. ⚠️ LESSON: backticks in a bash `-m` string trigger command-substitution (ran a stray `git clean`, refused; mangled the message) — use `git commit -F <file>` for multi-line/special-char messages. NEXT (Slice B): wire `ensemble merge --resolver <agent>` (adapter→closure bridge + resolver prompt) + `ensemble run --merge` auto-land via `Decision::Landed`.
- 2026-06-20 — **ExecAdapter per-command TIMEOUT LANDED @70f0fc1** (codex+claude LGTM, 2 gate rounds; two-phase spec Phase-1 STEP 5 — the opencode/CLI hang surfaced during the spec review). `ExecAdapter::run` had NO deadline (`wait_with_output`), so a CLI turn that never returns hung the whole governed run forever — the conductor's `max_task_secs` only fires at round BOUNDARIES, so it can't rescue a single wedged `run()`. Now: stdout+stderr drained on reader threads started BEFORE stdin is touched; the prompt delivered on a dedicated WRITER thread; the main thread polls `try_wait()` to a per-command deadline (default 600s, `with_timeout` override → future per-agent crew.toml knob, item 6); on timeout `kill_tree` kills the child → `Flaked("timed out after Ns")` so the reviewer is excluded via `on_flake`. Gate hardened the lifecycle: (r1-#1) Windows kill ordering — `child.kill()` killed `cmd.exe` before `taskkill /T` could enumerate the grandchild CLI → surviving CLI held the stdout pipe → reader join hung; fixed by `taskkill /F /T` FIRST then `child.kill()`. (r1-#2) the stdin write was outside the deadline and before the readers — a CLI that never drained stdin while flooding stdout could deadlock before the timeout loop; fixed by readers-first + writer-thread so every join (writer+2 readers) is reached only after kill+reap. (r1-#3 non-blocking) a Unix process-group kill (`process_group(0)` + `kill -KILL -<pid>`) was tried but MISBEHAVED under the WSL test harness (SIGKILL'd the test runner!) — reverted to `child.kill()` on Unix with the residual descendant-pipe limitation documented honestly. 2 hermetic tests (sleep/ping timeout→Flake <4s; echo fast-path captured). Closes the firewall-A follow-up "robust test-command timeout" for the adapter path. NEXT Phase-1: (2b) the `ensemble merge` AI-resolver round on conflict; (3) the `ensemble mcp` crew API.
- 2026-06-20 — **per-run JOURNAL LANDED @51d94de** (codex+claude LGTM; two-phase spec Phase-1 STEP 2 done). After a worktree run, `conductor::run_in_repo` writes the blackboard transcript (implementer result, each round's test result, reviewer verdicts, findings) + ONE terminal `decision` to `<repo>/.ensemble/runs/<slug>.jsonl` (JSONL) so the operator can REPLAY the collaboration — the visibility the Phase-1 live-proof endpoint needs. New `src/journal.rs`: `Entry` (serde internally-tagged `Msg(Message)`/`Decision`, reuses `blackboard::Message` now `Serialize/Deserialize`), pure `render`, `parse`, `write_run`, `journal_path`. `Worktree::slug()` exposed. Write is **best-effort** (`let _ =` — a journal failure never changes the run outcome) and captures the FINAL blackboard incl. the commit-failure downgrade. Covers run/run-many/dispatch (single funnel). Gate hardened it: (r1) `fs::write` truncated an existing journal — the worktree seq counter is process-local, so re-running the same task in a LATER process reused the slug and clobbered the prior record → `write_run` now create-news + disambiguates to `<slug>.N.jsonl` (never overwrites); slug path-safety via `sanitize_slug` (confine a public-API slug to one safe basename in `.ensemble/runs`, no sep/`..`, no truncation that could chop the seq); (r2) a hedged "won't compile" finding was a false positive (`read_since` returns `&[Message]`) — rebutted with the signature + the green suite, then LGTM. 6 journal unit tests + 1 integration test through real `run_in_repo`; full suite 89 lib + integration green. claude noted 2 cosmetic non-blockers (empty `detail` on a branchless land; Windows reserved-device slug names — both unreachable from the internal `-<seq>` path). NEXT Phase-1 slices: (3) `ensemble mcp` crew-participation API (the live-CLI-as-crew-member keystone); (2b) the `ensemble merge` AI-resolver round on conflict; (5) opencode per-command timeout.
- 2026-06-20 — **`ensemble merge` LANDED @68337ca** (codex+claude LGTM, 2 safety gate rounds; two-phase spec Phase-1 STEP 1 done — the #1 single-machine blocker). `repo_sync::merge_branch(repo, branch, into)`: ff or true-merge a kept `ensemble/<slug>` branch onto a target; a CONFLICT is NEVER auto-resolved — it aborts (worktree restored) + returns the conflicting paths. `ensemble merge <branch> [--into <t>] [--repo <p>]` CLI: exit 0 landed / 3 conflict / 1 error. The gate hardened it heavily (it lands onto main): PREFLIGHT refuses a dirty/already-merging worktree; EVERY failure path aborts any in-progress merge AND surfaces an abort failure as Err (no silent `let _ = merge --abort` anywhere) so no path leaves `into` half-merged; a failed conflict-query is fatal. 4 hermetic tests (ff / diverged-clean / conflict-abort-and-clean / dirty-refused) + empirical CLI smoke (ff→exit0, conflict→exit3+clean tree). NEXT slice: the AI-resolver round on conflict (operator's choice — spawn a CLI to attempt resolution, escalate if it still conflicts).
- 2026-06-20 — **agy adapter FIXED on Windows @6d5af89** (codex+claude LGTM, 2 gate rounds). `ensemble agent agy` always Flaked at the 180s timeout; the operator pointed at the proven `agy_pty.py` (phantom-mesh local-ai skill) — comparing it surfaced TWO real AgyAdapter bugs: (1) it omitted `--print-timeout <N>s` before `-p` (agy stalls under a PTY on the MCP-init/cold-auth path without it — antigravity-cli#76); (2) it waited only for the master's EOF, but portable-pty's Windows ConPTY master never EOFs after agy exits, so it hung to 180s AND discarded the answer agy had produced. Fix: agy_argv prepends `--print-timeout` (clamped strictly below the wall-clock); the read loop polls child-EXIT (not EOF) + a deterministic done-signal drain + reap + poison-safe locks; a genuine hang→Flaked (no salvaging a killed partial). **Empirically: agy now returns PONG in ~7s.** Residual Windows ConPTY reader-linger is an honestly-documented portable-pty limit (the pywinpty agy_pty.py driver is the fully-robust future backend). Also surfaced: opencode via ExecAdapter can HANG (no per-command timeout — a Phase-1 robustness gap, also flagged by the spec gate).
- 2026-06-20 — **OSS onboarding tick B LANDED @58e348b** (codex+claude LGTM). `ensemble up` — the quick-start: resolve the tailnet-only bind (E), print the mesh (D), then serve in the foreground until Ctrl-C. Pure `mesh::render_up` (banner + indented mesh + footer) unit-tested; `up_cmd` is thin glue. **Empirically on z13:** prints `ensemble up — serving on 100.87.70.65:7878 / local CLIs : codex,claude,opencode,agy / tailnet : (none discovered) / (serving… Ctrl-C to stop)`. claude caught a misleading flush comment (Rust stdout is LineWriter-flushed on `\n`, unlike C stdio) → fixed. **Core onboarding (up → see mesh → serve) now works.** Remaining: C `serve --install-service` (per-OS), A distribution.
- 2026-06-20 — **OSS onboarding tick D LANDED @50a0909** (codex+claude LGTM). `ensemble mesh` — read-only status view: this node's AI CLIs on PATH + each discovered tailnet peer → the agents it hosts. New `discovery::discover_mesh` (host→agents; `discover_agent_hosts` refactored to `build_agent_hosts(&discover_mesh())` — both gates confirmed behavior-identical), `doctor::present_clis`, pure `mesh::render_mesh`. Empirically on z13: `ensemble mesh` → "local CLIs : codex, claude, opencode, agy / tailnet : (none discovered)" in ~1.5s. Next OSS tick: B `ensemble up`.
- 2026-06-20 — **OSS onboarding tick E LANDED @ee09598** (codex+claude LGTM). `ensemble serve` now defaults to binding the node's **tailnet IPv4** (reachable only over the tailnet) instead of `0.0.0.0`; loopback fallback when there is no tailnet IP (NEVER widens to 0.0.0.0); `--bind` still overrides. New `discovery::parse_self_ips`/`self_tailscale_ips` (read `Self.TailscaleIPs` from the bounded status) + pure `serve::resolve_bind` (3 cases, unit-tested). **Empirically verified on z13:** `curl 100.87.70.65:7878/health` → all 4 agents; `curl 127.0.0.1:7878/health` → refused (truly tailnet-only). Re-gated SAFELY (codex from empty temp cwd). First tick of the operator's OSS-onboarding initiative (spec `docs/specs/2026-06-20-oss-onboarding-design.md`; next: D `ensemble mesh`).
- 2026-06-20 — **Discovery probe connect-timeout FIXED @5757152** (codex+claude LGTM). LIVE deployment surfaced it: once the real tailnet was up, `ensemble nodes` took ~30s because ureq's request `.timeout(2s)` does NOT bound the TCP connect — a single idle iOS/Android peer silently dropping :7878 hung ~30s and, since discovery joins on the SLOWEST parallel probe, dominated the whole fanout (`tailscale status --json` itself is 0.19s). Fix: `probe_agents` builds a ureq `Agent` with `.timeout_connect(800ms).timeout(2s)`. TDD: a probe to RFC5737 TEST-NET-1 (192.0.2.1, unrouted) must return <3s — reproduced the 30s hang in WSL, now <1s; full lib suite 30s→0.8s. (Discovered while bringing up a real multi-device tailnet; explicit `--node <host>` skips discovery so it never blocked deploy testing.)
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
