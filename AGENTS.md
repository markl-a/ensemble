# Project: ensemble

## What This Is

`ensemble` is a Rust 2021 CLI that turns different-vendor AI coding CLIs into a local-first, governed dev crew. It orchestrates implement/review/debug roles, keeps a mediated blackboard, can run work in git worktrees, and can federate agent execution across Tailscale peers.

Do not trust the README status block as current: it still says Phase 0 / not usable. For current progress, prefer `docs/AUTONOMOUS-BACKLOG.md`, especially the most recent log entries.

## Tech Stack

- Rust 2021 single-crate CLI, binary name `ensemble`.
- Key dependencies: `serde`, `serde_json`, `toml`, `toml_edit`, `thiserror`, `tiny_http`, `ureq`, `rusqlite` with bundled SQLite, `portable-pty`, `fs2`, `ctrlc`, `tempfile`.
- External CLIs the app drives when available: `codex`, `claude`, `opencode`, `agy`, plus `tailscale` for discovery/federation.

## Project Map

- `src/main.rs`: CLI dispatch and IO shell for `run`, `run-many`, `agent`, `codex`, `claude`, `opencode`, `serve`, `up`, `mesh`, `nodes`, `doctor`, `mcp`, `watch`, `supervise`, `steer`, `abort`, `merge`, `dispatch`, and `ledger`.
- `src/conductor.rs`: governed role pipeline, test gate, no-progress breaker, live observer/control integration.
- `src/crew.rs`: `crew.toml` schema: pipeline, roles, gate policy, optional test command, per-agent `node`, `backup`, `args`, and `timeout`.
- `src/adapter.rs`, `src/exec_adapter.rs`, `src/agy_adapter.rs`, `src/remote_adapter.rs`: local and remote agent execution.
- `src/serve.rs`, `src/discovery.rs`, `src/wire.rs`: Tailscale HTTP agent host and discovery.
- `src/mcp.rs`, `src/mcp_install.rs`, `src/board.rs`, `src/ledger.rs`, `src/worktree.rs`: MCP crew-participation API and persistent repo-local coordination state.
- `src/supervise.rs`, `src/supervisor.rs`, `src/ndjson.rs`, `src/journal.rs`: live supervision feeds, supervisor recommendations, control feeds, and run journals.
- `src/repo_sync.rs`: git merge, bundle, and work-return safety logic.
- `tests/`: hermetic and integration coverage for pipeline, cross-machine protocol, ledger/dispatch, firewall behavior, and live smoke.

## Current State

- Phase-1 core is code-complete: merge, journal, AI resolver, exec timeout, and the full MCP crew API are landed.
- MCP API includes crew tools (mesh, board read/post, worktree, enqueue, claim, merge, complete, fail, run), team/control tools, and the Task 6 `ensemble_supervise` tool.
- Governed local runs and at least a small tailnet cross-machine proof have been demonstrated in the backlog log.
- Single-machine live supervision is complete: observe (`run --watch` / `watch`), interrupt (`abort [--hard]`), adjust (`steer`), and configure (`[agents.<name>] args` / `timeout`).
- Active operator-requested Phase-1 plan: `docs/plans/2026-06-22-single-machine-team-cli-phase1.md` designs `ensemble [ensemble options] codex|claude|agy|opencode [vendor args...]`, shared team state, MCP/team control tools, supervisor checks, smoke tests, and operator acceptance flow.
- Phase-1 user workflow and personal acceptance runbook: `docs/RUNBOOK-single-machine-team-cli.md`.
- Current progress in that plan: Task 1 (team/member contract), Task 2 (`ensemble team status|say|inbox`), Task 3 (MCP `ensemble_team_status|say|inbox`, `ensemble_watch`, `ensemble_steer`, `ensemble_abort`), Task 4 (`ensemble [options] codex|claude|opencode [vendor args...]`), Task 5 (`ensemble [options] agy [vendor args...]`), Task 5.5 (`--confirm-policy ask|approve|deny`), Task 6 (`ensemble supervise` / MCP `ensemble_supervise`), Task 7 (single-machine smoke extension), Task 8 (operator runbook developer dry-run), Task 9 (user-level install/uninstall readiness), and Task 10 (launcher syntax cleanup) are complete and verified. Post-review, live-run MCP tools use the same `.ensemble/stream` and `.ensemble/control` feeds as the existing CLI/conductor, even when `ensemble mcp --team <name>` is set; only the team board/status/inbox tools are team-scoped. The Task 4 launchers reuse `mcp_install` rendering, include `--team <name>` in generated MCP server args, support `--print-config` dry-run, pass all tokens after the AI CLI name to that vendor, reject the old `--` separator form, and show the resolved confirmation policy. `ensemble agy` now has two modes: without post-`agy` prompt flags it launches the interactive Antigravity UI in the target repo with inherited stdio; with post-`agy` `--prompt` / `-p` / `--print` or front-side `--json` / `--print-prompt`, it runs the bounded one-shot team wrapper, includes recent team-board context, posts success as `result`, posts failures/timeouts as visible `flake` board messages, and forwards remaining post-`agy` vendor args into the PTY wrapper. `--confirm-policy approve` adds agy's verified non-interactive permission approval flag in both modes. `ensemble supervise` collects stream/board/git evidence, asks a local supervisor agent for strict JSON, posts an advisory board result by default, and writes steer/critical-abort controls only when the parsed recommendation and flags allow it. `ensemble_supervise` is now advertised by the real MCP server. `scripts/smoke.ps1` now performs team board checks, launcher dry-runs, bounded agy wrapper checks, optional supervisor advisory capture, artifact recording, and comma-separated reviewer parsing; `-Reviewers claude,agy -TimeoutSecs 240` currently exits 1 because local `agy` produces empty reviewer output, with the exact failure recorded in artifacts. `scripts/install.ps1` installs `ensemble.exe` to `%LOCALAPPDATA%\ensemble\bin` and updates User PATH; `scripts/uninstall.ps1` removes the PATH entry/binary and can optionally remove ensemble's MCP entries via `ensemble mcp uninstall`. The runbook dry-run found and fixed the need for a minimal scratch `crew.toml`; operator-only live terminal validation remains pending.
- Task 10.5 is now implemented after the original status line above: `ensemble codex`, `ensemble claude`, `ensemble opencode`, and interactive `ensemble agy` launch through a controlled PTY by default. The controlled launcher tails `.ensemble/control/<member>.ndjson`; `ensemble steer <member> "<prompt>"` sends Escape, waits briefly, then types the corrective prompt and Enter; `ensemble abort <member>` sends Escape; `ensemble abort <member> --hard` also kills the launched child process. The deterministic acceptance script includes an isolated fake Codex launcher check for this default path. Real vendor live-interruption behavior still needs the operator terminal pass because each vendor decides how Escape affects an in-flight generation or menu.
- Verified for Task 10.5 on 2026-06-22: `cargo test --lib controlled --target-dir D:\tmp\ensemble-target-controlled-5`, `cargo test --bin ensemble --target-dir D:\tmp\ensemble-target-controlled-bin-4`, and `pwsh -NoProfile -File scripts\acceptance-single-machine.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-acceptance-controlled -TargetDir D:\tmp\ensemble-target-controlled-release -AgyTimeoutSecs 1` all passed.
- Automation requirement from the operator: any CLI path ensemble drives automatically must not depend on hidden interactive confirmations. Use non-interactive permission/confirmation flags where available, expose an explicit policy, or time out and write a visible team-board flake. Do not count a stuck vendor prompt as a valid review or smoke result. On this machine, `opencode --help` does not expose a stable approve/deny confirmation flag, so ensemble rejects opencode `--confirm-policy approve|deny` instead of pretending it can operate those choices.
- Next major work is still tracked in `docs/AUTONOMOUS-BACKLOG.md`: OSS onboarding tick C (`serve --install-service` / `--uninstall-service`), distribution tick A, cross-machine supervision control routes, and release cleanup.
- Release blockers include `Cargo.toml` version still being `0.0.0` and the stale README.
- Verified on 2026-06-22: the WSL path `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test` passes, and WSL `cargo clippy --all-targets -- -D warnings` passes. Native Windows `cargo test` can fail in `repo_sync` restore tests because temp git repos inherit CRLF checkout behavior (`main-edit\r\n` vs expected `main-edit\n`).

## Common Commands

- Build/check: `cargo check`
- Install current build for this Windows user: `pwsh scripts\install.ps1`
- Uninstall current user install: `pwsh scripts\uninstall.ps1`
- Optional MCP cleanup during uninstall: `pwsh scripts\uninstall.ps1 -RemoveMcpConfig -Repo <repo> -Clients codex,claude,opencode`
- Primary test path: in WSL, `cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`
- Native Windows smoke check: `cargo check --target-dir D:\tmp\ensemble-target`
- Format check: `cargo fmt --check`
- Lint: in WSL, `cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo clippy --all-targets -- -D warnings`
- Avoid a locked native target dir by using a separate target directory, for example `cargo test --target-dir D:\tmp\ensemble-target` on Windows or `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test` in WSL.
- Quick local readiness: `ensemble doctor`
- Foreground node host: `ensemble up`
- Tailnet mesh view: `ensemble mesh`
- Register MCP for a CLI: `ensemble mcp install --client <claude|codex|opencode> --repo <path> --team <name> --crew <path>`
- Remove ensemble's MCP entry for a CLI: `ensemble mcp uninstall --client <claude|codex|opencode> --repo <path>`
- Start an MCP-capable controlled team member: `ensemble --repo <path> --team <name> --confirm-policy ask --print-config codex|claude|opencode [vendor args...]` to preview, then omit `--print-config` to write the MCP config and launch the vendor CLI under the controlled PTY.
- Start the controlled agy UI: `ensemble --repo <path> --team <name> --confirm-policy ask agy [agy args...]`. Run a bounded agy team wrapper: `ensemble --repo <path> --team <name> --timeout 30 --confirm-policy ask --json agy --prompt <text> [agy args...]`; it should post either `result` or `flake` to the team board.
- Intervene in a controlled interactive member: `ensemble steer <member> "<prompt>" --repo <path>` sends Escape plus the corrective prompt, `ensemble abort <member> --repo <path>` sends Escape, and `ensemble abort <member> --hard --repo <path>` also kills the launched child process.
- Ask for a supervisor check: `ensemble supervise <watch-name> --repo <path> --team <name> --agent claude --json`; add `--apply-steer` or `--abort-on-critical` only when the operator wants parsed recommendations to mutate the control feed.
- Phase-1 user workflow runbook: `docs/RUNBOOK-single-machine-team-cli.md`.
- Deterministic single-machine Phase-1 acceptance: `pwsh scripts\acceptance-single-machine.ps1` (use `-NoBuild -TargetDir <target>` to reuse an existing release binary). It validates team board, auto member names, launcher config previews, a controlled fake Codex launcher, controlled agy launch, bounded agy wrapper, MCP stdio team/control tools, watch, steer, and abort without requiring a live vendor UI to click prompts.
- Single-machine multi-AI governed smoke: `pwsh scripts/smoke.ps1` (defaults to codex implementer + claude reviewer, local-only `--no-discover`, test gate, watch feed, auto-merge in a throwaway repo, and artifacts under `<SmokeRoot>\smoke-artifacts`). Use `-PreflightOnly` for team/launcher/agy-wrapper checks without a governed run. Use `-Reviewers claude,agy` to require two reviewer approvals.
- Verified on 2026-06-22: `pwsh -NoProfile -File scripts\smoke.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-smoke-task7-full2 -TargetDir D:\tmp\ensemble-smoke-task7-target -TimeoutSecs 180 -AgyTimeoutSecs 5` passed with codex -> test gate -> claude -> `LANDED`, supervisor advisory returned `on_track`, and `RESULT.txt` merged onto `main`. `pwsh -NoProfile -File scripts\smoke.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-smoke-task7-agy-review-240b -TargetDir D:\tmp\ensemble-smoke-task7-target -Reviewers claude,agy -TimeoutSecs 240 -AgyTimeoutSecs 5 -SkipSupervisor` ran bounded and exited 1 because local `agy` produced empty reviewer output, leaving watch/board/git artifacts that show quorum stayed 1/2.

## Development Rules

- Chat with the operator in Traditional Chinese. Keep code, comments, commit messages, and durable docs in English unless the surrounding file already uses another convention.
- Use TDD for behavior changes. Add or update tests before relying on implementation.
- Before landing non-trivial work, run the project checks and perform the documented double-gate review: at least two different AIs must both return `VERDICT: LGTM`.
- Never fake green: failed tests, flaked agents, missing reviewers, or unclear verdicts must not count as approval.
- Do not spawn autonomous loops. If a real operator decision is needed, stop and leave a clear note.
- Do not use `git add -A`. Stage specific files only; this repo has a documented history of stray build directories being swept into commits.
- Keep commits atomic. The project convention records Claude co-authoring on landed commits; check the current backlog before committing.
- Treat Tailnet IPs, machine names, internal paths, and secrets as sensitive when preparing public-facing docs or release artifacts.

## Context Loading Order

1. Read `docs/AUTONOMOUS-BACKLOG.md` for the latest truth and next task.
2. Read the relevant spec/plan under `docs/specs/` or `docs/plans/`.
3. Read the source modules and tests involved before editing.
4. For CLI behavior, inspect `src/main.rs` for existing argument parsing style before adding flags.
5. For safety-critical git, MCP, ledger, or supervision changes, find an existing hermetic test pattern before writing new code.
