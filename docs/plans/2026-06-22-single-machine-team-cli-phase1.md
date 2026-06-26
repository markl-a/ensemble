# Phase 1 Plan — Single-Machine Team CLI

> **For agentic workers:** build this task-by-task. Pure contracts and parsers are TDD'd in `src/*`.
> IO shells in `src/main.rs` are smoke-tested with real CLIs. Do not start by rewriting the conductor.

## Goal

Make a single Windows/macOS/Linux machine feel like a local AI-CLI team:

```text
ensemble codex
ensemble claude --continue
ensemble agy --continue
ensemble opencode --continue
```

Each command starts or attaches that vendor CLI to the same repo-local ensemble team. The active user's
CLI can inspect the other running members, ask a supervisor subagent whether a run is drifting, steer a
run or ensemble-launched member with a new prompt, abort a member or governed run, and let members communicate through the shared
board.

## Phase-1 Definition

Phase 1 is complete when a single operator can personally run the four local CLIs under `ensemble`,
observe their shared team state, exchange board messages between them, run a governed task, and intervene
with `watch`, `steer`, and `abort` without relying on Tailscale or another machine.

### Must Have

- `ensemble [ensemble options] codex|claude|opencode|agy [vendor args...]` is the launcher entry point.
- Every launched member uses a stable member id, for example `codex@node-a`.
- Every member launched in the same repo and team writes to the same `.ensemble/` state.
- The main user-facing CLI can inspect team status and board messages.
- The main user-facing CLI can send a team message and read replies.
- The main user-facing CLI can steer or abort a live governed run or ensemble-launched interactive
  member by name.
- A supervisor check can review the current stream, board, git status, and diff, then recommend
  `on_track`, `steer`, or `abort`.
- The local smoke path proves at least `codex -> test gate -> claude -> LANDED -> merged`.
- The operator acceptance path also tests `agy` and `opencode` on the same machine, with timeouts so one
  flaky CLI never blocks the whole team.

### Honest Limits

- Phase 1 does not promise visibility into a vendor CLI's private hidden subagents unless that CLI emits
  observable output, MCP calls, logs, or tool events.
- Phase 1 does not require cross-machine control. Tailscale and `serve` remain Phase 2/fleet work.
- Phase 1 does not require `agy` to be a full MCP client. If `agy` cannot consume MCP tools directly, the
  first version wraps it with prompt context and posts its final answer back to the team board.

## Current Foundation

Already present and should be reused:

- `ensemble mcp` with board read/post, worktree, merge, complete/fail, and `ensemble_run`.
- `ensemble mcp install --client <claude|codex|opencode>` for MCP-capable clients.
- `ensemble mcp uninstall --client <claude|codex|opencode>` to remove only ensemble's MCP entry.
- `ensemble watch <name> --follow` for live run feeds.
- `ensemble steer <name> "<prompt>"` and `ensemble abort <name> [--hard]` for control feeds.
- `ensemble all "<prompt>"` draft/council path if present in the worktree.
- `scripts/smoke.ps1` for single-machine governed CLI smoke.
- `crew.toml` role pipeline with per-agent `timeout`, `args`, and `on_flake`.

## Architecture Decisions

- **Team identity:** team state is repo-local. Default team is `default`; state remains under `.ensemble/`.
  Add a team name only where needed for UX and future multi-team separation.
- **Member identity:** default member id is `<client>@<short-host>`, reusing the existing auto-name rule.
  A user may override it with `--name`.
- **MCP first:** `codex`, `claude`, and `opencode` should use MCP when available because that gives real
  board/tool interaction inside the live CLI.
- **Agy fallback:** `agy` can launch as a direct interactive UI when the operator wants to use
  Antigravity manually. When the operator supplies `agy --prompt` / `agy -p`, it runs as a bounded
  adapter-backed team turn, receives board context in its prompt, and posts its final response or flake
  to the board.
- **Control is file-plane:** supervision uses existing `.ensemble/stream/*.ndjson` and
  `.ensemble/control/*.ndjson`. Governed runs consume those feeds at round boundaries, and the default
  interactive launchers consume the same feeds through a controlled PTY that maps `steer` to
  Escape plus the corrective prompt and maps hard `abort` to child-process termination.
- **Supervisor is explicit:** do not hide an always-on judge inside every command. Provide an explicit
  `ensemble supervise ...` / MCP tool path so the active CLI can ask for a drift check when useful.
- **Timeouts are mandatory for automation:** `agy --prompt` and `opencode` automated paths must be
  configured with bounded per-agent timeouts in smoke and acceptance flows.
- **No hidden confirmation stalls:** any CLI path that ensemble drives automatically must either use a
  non-interactive permission/confirmation mode, expose an explicit yes/no/ask policy, or time out and
  write a visible team-board flake. A run that is waiting in a hidden vendor prompt does not count as a
  valid review, launcher proof, or acceptance signal.

## Public CLI Shape

### Member Launchers

```text
ensemble [--team default] [--member <member>] [--repo <path>] [--confirm-policy ask|approve|deny] [--print-config] codex [vendor args...]
ensemble [--team default] [--member <member>] [--repo <path>] [--confirm-policy ask|approve|deny] [--print-config] claude [vendor args...]
ensemble [--team default] [--member <member>] [--repo <path>] [--confirm-policy ask|approve|deny] [--print-config] opencode [vendor args...]
ensemble [--team default] [--member <member>] [--repo <path>] [--timeout <secs>] [--confirm-policy ask|approve|deny] [--print-prompt] [--json] agy [--prompt <text>|-p <text>|vendor args...]
```

Expected behavior:

- Resolve `repo` to an absolute path.
- Resolve `member` to an explicit `--member` / legacy alias `--name` before the AI CLI name, or
  `<client>@<short-host>`.
- Treat every token after the AI CLI name as that vendor CLI's own arguments. The old `--` separator
  form is removed.
- For `agy`, post-`agy` `--prompt <text>`, `--print <text>`, or `-p <text>` is extracted as the
  requested team turn and wrapped with board context; other post-`agy` args such as `--continue` pass
  through to Antigravity.
- For `agy` without a post-`agy` prompt flag and without `--json` / `--print-prompt`, launch the
  interactive Antigravity UI in the target repo through the same controlled PTY path as other
  interactive members.
- Resolve `--confirm-policy` to `ask` by default; apply verified vendor permission flags only where
  supported, and fail fast when the requested policy is unsupported for that client.
- Ensure `.ensemble/` exists.
- For MCP-capable clients, ensure the vendor's MCP config points at
  `ensemble mcp --repo <repo> --name <member> --crew <repo>/crew.toml`.
- Launch the vendor CLI in controlled interactive mode by default.
- Print a short banner showing member id, repo, team, board path, and useful commands.

### Team Commands

```text
ensemble team status [--repo <path>] [--team default] [--json]
ensemble team watch  [--repo <path>] [--team default] [--follow]
ensemble team say    "<message>" [--repo <path>] [--from <member>]
ensemble team inbox  [--repo <path>] [--since <n>] [--json]
```

Expected behavior:

- `status` summarizes known members, recent board posts, live streams, control feeds, and ledger counts.
- `watch` tails the shared board plus live streams in a human-readable view.
- `say` appends a board message from the operator or active member.
- `inbox` reads board messages with a cursor for scripts or MCP tools.

### Supervisor Commands

```text
ensemble supervise <name> [--repo <path>] [--agent claude] [--since <n>] [--json]
ensemble supervise <name> --apply-steer [--agent claude]
ensemble supervise <name> --abort-on-critical [--agent claude]
```

Expected behavior:

- Read live stream events for `<name>`.
- Read recent board messages.
- Read `git status --short` and a bounded diff summary from the active worktree/repo.
- Ask the selected supervisor agent for a structured recommendation:
  `on_track`, `steer`, `abort`, or `needs_human`.
- By default, only print the recommendation and post it to the board.
- `--apply-steer` may append a `ControlCmd::Steer`.
- `--abort-on-critical` may append a hard abort only when the supervisor explicitly marks the issue
  critical.

### MCP Tool Additions

Expose the team/control surface to MCP clients so the active user CLI can supervise the team without
shelling out:

- `ensemble_team_status`
- `ensemble_team_say`
- `ensemble_team_inbox`
- `ensemble_watch`
- `ensemble_steer`
- `ensemble_abort`
- `ensemble_supervise` (implemented in Task 6; advertised by the real MCP server)

These are additive. Existing MCP tools remain stable.

## Development Plan

**Progress 2026-06-22:**

- Task 1 is implemented. `src/team.rs` now owns the pure team/member contract,
  `mcp_install::default_member_name` delegates to it, and the new helpers are re-exported from
  `src/lib.rs`. Verified with `cargo test --lib team`, `cargo test --lib mcp_install`,
  `cargo check --target-dir D:\tmp\ensemble-target-team`, and WSL
  `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib`.
- Task 2 is implemented. `ensemble team status|say|inbox` now uses the resolved team session,
  supports default and named teams, emits stable JSON for status/inbox, and rejects malformed team
  flags before writing. Verified with `cargo test team`, `cargo test --lib board`,
  `cargo test --lib mcp_install`, `cargo check --target-dir D:\tmp\ensemble-target-team`, WSL
  `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib`, and a temp-repo CLI smoke under
  `%TEMP%\ensemble-team-task2-1a3308da95bf43cab1ebe3fe2a13281e`.
- Task 3 is implemented for MCP team/control tools. `ensemble mcp` now carries a default team via
  `--team`; MCP clients can call team-scoped `ensemble_team_status`, `ensemble_team_say`, and
  `ensemble_team_inbox`, plus live-run `ensemble_watch`, `ensemble_steer`, and `ensemble_abort` on
  the same stream/control feeds used by the existing CLI/conductor. The write tools attribute actions
  to the MCP server identity (`ctx.name`), reject client-supplied author fields, and bounded read tools
  return cursors. A Codex subagent and Claude Code review both caught a named-team live-feed mismatch;
  it is fixed and covered by tests. Local `agy` returned `Empty` even for a short prompt, and
  `opencode run` timed out even for a short prompt in a disposable copy, so neither produced a usable
  review. Verified with `cargo test --lib mcp`, `cargo test team`,
  `cargo check --target-dir D:\tmp\ensemble-target-team`, `cargo build --target-dir
  D:\tmp\ensemble-target-team`, real stdio MCP smokes under
  `%TEMP%\ensemble-mcp-task3-59883602c7924a788b56425956a772a0` and
  `%TEMP%\ensemble-mcp-task3-reviewfix-4ce186c9cc564e7a9531f6e3c113dc20`, and WSL
  `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib`. Native Windows
  `cargo test --lib --target-dir D:\tmp\ensemble-target-team` still has unrelated `repo_sync`
  CRLF restore assertion failures.
- Task 4 is implemented for MCP-capable launchers. `ensemble [options] codex|claude|opencode [vendor args...]`
  parses `--repo`, `--team`, `--member` / `--name`, and `--print-config` before the AI CLI name, then
  passes all tokens after the AI CLI name through to the vendor. The launchers resolve a stable member id,
  reuse `mcp_install::render_merged`, write the vendor MCP config only outside `--print-config`, create
  the resolved team root, print the resolved member/repo/team/crew/board, then start the vendor CLI.
  `mcp_install::InstallParams` now includes `team`, and generated MCP server args include
  `--team <name>` so named-team launcher sessions do not fall back to `default`. Verified with
  `cargo test --bin ensemble member_launcher`, `cargo test --bin ensemble`, `cargo test --lib
  mcp_install`, `cargo test --lib mcp`, `cargo test --lib team`, `cargo check --target-dir
  D:\tmp\ensemble-target-team`, dry-run smoke for all three launchers under
  `%TEMP%\ensemble-launcher-task4-5564545ea4bf44b38e113d1a6a09fc02` with no config/team files
  created, and a non-interactive real launch smoke using vendor `--version` args for all three under
  `%TEMP%\ensemble-launcher-task4-real-0c0fc3e1f2a84f849450b2f0e6b46224` with Codex isolated via a
  temporary `CODEX_HOME`. A later attempt to use live interactive windows and external CLI reviews
  showed the operator concern clearly: hidden confirmation/permission prompts are not reliable
  automation. Claude reviewed Task 4 successfully with non-interactive permissions and returned LGTM;
  local `opencode run` timed out after 180 seconds, and local `agy` returned `Empty`, so neither counts
  as a usable Task 4 review signal.
- Task 5 is implemented as a dual-mode `ensemble [options] agy [vendor args...]` launcher.
  Without post-`agy` prompt flags and without `--json` / `--print-prompt`, it launches Antigravity's
  interactive UI in the target repo with inherited stdin/stdout/stderr and the same team/member
  banner used by the other launchers.
  With post-`agy` prompt flags, it runs the bounded one-shot team wrapper.
  It parses `--repo`, `--team`, `--member` / `--name`, `--timeout <secs>`, `--print-prompt`, and
  `--json` before `agy`; extracts `agy --prompt <text>` / `agy -p <text>` after `agy`; builds a prompt with
  recent team-board context and explicit non-interactive confirmation guidance; runs `AgyAdapter` with
  the requested wall-clock timeout and post-`agy` vendor args; posts successful output to the team board as `result`; and posts
  visible failures to the same board as `flake` before exiting with the adapter's failure code. Verified
  with `cargo test --bin ensemble`, `cargo test --lib team`, `cargo check --target-dir
  D:\tmp\ensemble-target-team`, WSL `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test --lib`, and a
  temp-repo smoke under `%TEMP%\ensemble-agy-task5-ac431a31d5134126becb86cb3cdfe2d5` where
  `--print-prompt` did not mutate the board and a real local `agy` short-timeout flake wrote a visible
  `agy@smoke [flake]` message instead of hanging.
- Task 5.5 confirmation-policy hardening is implemented. The launchers accept
  `--confirm-policy ask|approve|deny` before the AI CLI name. `ask` is the default and keeps manual
  choice handling with no hidden automation. `approve` maps to locally verified non-interactive
  permission flags for Codex, Claude Code, and agy. `deny` maps to Codex read-only/no-ask mode and
  Claude Code `dontAsk`; agy receives an explicit deny instruction and remains bounded by timeout.
  `opencode --help` does not expose a stable confirmation flag on this machine, so `approve`/`deny`
  are rejected for opencode instead of silently pretending to work. This is not full PTY/expect menu
  automation; that remains a later live-session driver layer.
- Task 6 supervisor check is implemented. `ensemble supervise <name>` collects bounded stream
  evidence, recent team-board messages, `git status --short`, and `git diff --stat --no-ext-diff`,
  asks a local supervisor agent for a strict JSON recommendation, parses
  `on_track|steer|abort|needs_human`, and posts an advisory `supervise` board message by default.
  `--apply-steer` writes a control-feed steer only for a parsed `steer` recommendation with a prompt;
  `--abort-on-critical` writes a hard abort only for a parsed `abort` recommendation with
  `critical=true`. MCP now advertises `ensemble_supervise` from the real `ensemble mcp` server and
  uses the same policy through a configured `SupervisorRunner`. Verified with `cargo test --lib
  supervisor`, `cargo test --lib mcp`, `cargo test --bin ensemble`, `cargo check --target-dir
  D:\tmp\ensemble-target-team`, and a stdio MCP `tools/list` smoke confirming `ensemble_supervise`
  is advertised. A live `ensemble run --watch <name>` manual operator test is still part of
  acceptance.
- The expected user usage model and operator runbook are now stored in
  `docs/RUNBOOK-single-machine-team-cli.md`. It defines the package's daily workflow, MCP tool usage,
  supervisor flow, confirmation-policy rules, and personal acceptance checks. Developer dry-run and
  operator execution are still pending.
- Task 7 single-machine smoke extension is implemented. `scripts/smoke.ps1` now runs team board
  say/status/inbox checks, non-mutating launcher previews for `codex`, `claude`, `opencode`, and
  `agy`, a bounded `ensemble agy --prompt` wrapper turn that must post either `result` or `flake`, optional
  supervisor advisory capture, comma-separated reviewer parsing (`-Reviewers claude,agy`), isolated
  scratch `CODEX_HOME` for Codex dry-run previews, and artifact recording under
  `<SmokeRoot>/smoke-artifacts`. Verified default `codex -> test gate -> claude -> LANDED`; the
  `claude,agy` reviewer path is bounded and diagnostic-complete but currently exits 1 because local
  `agy` produces empty reviewer output, leaving the exact stream/watch/board/git artifacts.
- `scripts/acceptance-single-machine.ps1` adds a deterministic Phase 1 automated acceptance path. It
  creates a scratch repo and validates `doctor`, team status/say/inbox, auto member names, launcher MCP
  config previews, direct `agy` launch via `agy --help`, bounded `agy --prompt` visible result/flake,
  real `ensemble mcp` stdio `tools/list` plus team/watch/steer/abort calls, direct CLI `watch`,
  `steer`, and `abort`. It intentionally does not prove that vendor UIs expose MCP tools; that remains
  part of the operator live terminal pass.
- Task 8 operator acceptance runbook is written and developer dry-run. The dry-run found and fixed a
  real runbook gap: a scratch repo needs a minimal `crew.toml` before `ensemble run` can work outside
  this repository. Automated dry-run coverage now includes scratch setup, team board, launcher
  previews, bounded agy flake visibility, raw MCP stdio posts from two member identities, governed
  run/watch/merge, supervisor advisory, and control-feed writes for steer/abort. The remaining gap is
  operator-only live interactive validation in separate terminals.
- Task 9 install/uninstall readiness is implemented for the current Windows user flow. `scripts/install.ps1`
  installs `ensemble.exe` into `%LOCALAPPDATA%\ensemble\bin`, updates User PATH, and runs
  `ensemble doctor`. `scripts/uninstall.ps1` removes the PATH entry and installed binary directory, and
  can optionally call `ensemble mcp uninstall` for Codex, Claude, and opencode before removing the
  binary. Verified with cargo checks, MCP install/uninstall scratch configs, scratch script
  install/uninstall, real user PATH install, and `D:\Projects\mix_swarm` launcher dry-runs.
- Task 10 launcher syntax cleanup is implemented. The launcher grammar is now
  `ensemble [ensemble options] <codex|claude|opencode|agy> [vendor args...]`: options before the AI CLI
  name are parsed by ensemble, and options after the AI CLI name are passed to that vendor. The old
  `--` separator form is rejected. `--member` is the preferred ensemble member-id flag; `--name` remains
  an alias only before the AI CLI name. `agy` now extracts post-`agy` prompt flags into the team prompt
  and carries the remaining vendor args into the PTY wrapper before the generated
  `--print-timeout -p <team prompt>` arguments. When no post-`agy` prompt flag is present, `agy`
  starts in direct interactive UI mode instead.
- Task 10.5 default controlled interactive launch is implemented. `ensemble codex`, `ensemble claude`,
  `ensemble opencode`, and interactive `ensemble agy` now start the vendor CLI under a controlled PTY
  by default. The launcher tails `.ensemble/control/<member>.ndjson`; `ensemble steer <member>
  "<prompt>"` sends Escape, waits briefly, then types the corrective prompt and Enter; `ensemble abort
  <member>` sends Escape; `ensemble abort <member> --hard` also kills the launched child process. The
  banner now prints the exact steer/abort commands for the resolved member. Verified with
  `cargo test --lib controlled --target-dir D:\tmp\ensemble-target-controlled-5`, `cargo test --bin
  ensemble --target-dir D:\tmp\ensemble-target-controlled-bin-4`, and a controlled PTY unit test that
  launches a real child process and returns its exit code. The deterministic acceptance script also
  passed with a controlled fake Codex launcher and controlled agy launch:
  `pwsh -NoProfile -File scripts\acceptance-single-machine.ps1 -NoBuild -SmokeRoot
  D:\tmp\ensemble-acceptance-controlled -TargetDir D:\tmp\ensemble-target-controlled-release
  -AgyTimeoutSecs 1`. Real vendor live-interruption behavior still requires the operator acceptance
  pass because each vendor decides how Escape affects an in-flight generation or menu.

### Task 1: Define team/member contract

**Description:** Add pure helpers for resolving repo, team name, member id, board paths, and session
metadata. This is the foundation used by CLI wrappers and MCP tools.

**Acceptance criteria:**

- [x] Default team is `default`.
- [x] Default member name is `<client>@<short-host>`.
- [x] Host/member sanitization cannot escape `.ensemble/`.
- [x] Explicit `--name` overrides the default.

**Verification:**

- [x] `cargo test --lib team`
- [x] Existing `default_member_name` tests still pass.

**Dependencies:** None.

**Files likely touched:**

- `src/team.rs` or `src/supervise.rs`
- `src/lib.rs`
- `src/main.rs` tests if arg parsing remains local

**Estimated scope:** Medium.

### Task 2: Add `ensemble team status|say|inbox`

**Description:** Provide repo-local team observability and board messaging without starting a vendor CLI.

**Acceptance criteria:**

- [x] `ensemble team say "hello"` writes one board post attributed to `operator` by default.
- [x] `ensemble team inbox --since 0 --json` returns parseable messages and a cursor.
- [x] `ensemble team status --json` returns stable fields for repo, team, board length, ledger counts, and
  known live feeds.
- [x] Malformed flags fail with exit code 2 and no writes.

**Verification:**

- [x] Unit tests for pure render/shape functions.
- [x] Smoke in a temp repo:
  `ensemble team say "hello"` then `ensemble team inbox --json`.

**Dependencies:** Task 1.

**Files likely touched:**

- `src/team.rs`
- `src/main.rs`
- `src/lib.rs`
- `src/board.rs` if a small public helper is needed

**Estimated scope:** Medium.

### Task 3: Add MCP team/control tools

**Description:** Let active MCP-capable CLIs read team state, post messages, watch streams, steer, and
abort through tools instead of shell commands.

**Acceptance criteria:**

- [x] `tools/list` advertises the new tools with stable JSON schemas.
- [x] `ensemble_team_say` writes as server identity (`ctx.name`), never as a client-supplied author.
- [x] `ensemble_steer` and `ensemble_abort` validate target names and append to the same control feeds as
  CLI commands.
- [x] `ensemble_watch`/`ensemble_team_inbox` return bounded output with cursors.
- [x] Invalid input returns JSON-RPC `-32602`; IO failures return `-32603`.

**Verification:**

- [x] `cargo test --lib mcp`
- [x] Real stdio MCP smoke: initialize, tools/list, team_say, team_inbox, watch, steer, abort.

**Dependencies:** Tasks 1-2.

**Files likely touched:**

- `src/mcp.rs`
- `src/team.rs`
- `src/supervise.rs`

**Estimated scope:** Medium.

### Task 4: Add `ensemble codex|claude|opencode` launchers

**Description:** Add vendor launcher commands that join the repo-local team, ensure MCP wiring where
possible, then start the vendor CLI.

**Acceptance criteria:**

- [x] Commands exist in `USAGE`.
- [x] `--repo`, `--team`, `--member` / `--name`, and post-launcher vendor args parse predictably.
- [x] `--confirm-policy ask|approve|deny` parses predictably and only maps to verified vendor flags.
- [x] Launchers print the resolved member id and repo before starting the vendor CLI.
- [x] Launchers use the controlled interactive PTY by default and print the resolved `steer`/`abort`
  commands.
- [x] `--print-config` or equivalent dry-run mode shows what would be launched without mutating files.
- [x] The implementation reuses `mcp_install` rendering rather than duplicating per-client config logic.

**Verification:**

- [x] Pure parser tests for all three commands.
- [x] Dry-run smoke for all three clients.
- [x] Non-interactive real launch smoke for all three clients with vendor `--version` in a scratch repo.
- [ ] Full interactive manual launch of at least `ensemble codex` and `ensemble claude` in a scratch repo.

**Dependencies:** Tasks 1 and 3.

**Files likely touched:**

- `src/main.rs`
- `src/mcp_install.rs`
- `src/team.rs`
- tests in `src/main.rs`

**Estimated scope:** Medium.

### Task 5: Add `ensemble agy` launcher/team wrapper

**Description:** Provide an `agy` launcher that participates in the same team even if agy does not expose
the same MCP client surface as Codex/Claude/Opencode.

**Acceptance criteria:**

- [x] `ensemble [options] agy [vendor args...]` starts Antigravity's interactive UI under the
  controlled launcher when no prompt flag is provided.
- [x] `ensemble [options] agy --prompt <text>` starts a one-shot team turn with bounded timeout.
- [x] The prompt includes recent team board context.
- [x] The final response is posted to the board as `agy@<host>`.
- [x] If agy is missing or flakes, the board records a visible flake message.
- [x] The wrapper never blocks indefinitely.
- [x] `--confirm-policy approve` enables agy's verified non-interactive permission approval flag.
- [x] `--confirm-policy deny` is included in the bounded prompt contract and does not silently approve.
- [x] If agy enters an unhandled interactive choice/confirmation prompt, the wrapper exits via timeout
  and records that visible flake instead of silently waiting forever.

**Verification:**

- [x] Unit tests for prompt/context construction.
- [x] Adapter smoke with short timeout.
- [x] Manual `ensemble --repo <scratch> agy --prompt <text>` flake/result appears in
  `ensemble team inbox`.

**Dependencies:** Tasks 1-2.

**Files likely touched:**

- `src/main.rs`
- `src/agy_adapter.rs`
- `src/team.rs`

**Estimated scope:** Medium.

### Task 6: Implement supervisor check

**Description:** Add an explicit supervisor path that asks a chosen AI to inspect recent team/run evidence
and recommend whether the run is on track.

**Acceptance criteria:**

- [x] Supervisor prompt includes bounded stream excerpt, board excerpt, git status, and diff summary.
- [x] Output is parsed into `on_track`, `steer`, `abort`, or `needs_human`.
- [x] Default mode is advisory only and posts the result to the board.
- [x] `--apply-steer` writes a steer only for a parsed `steer` recommendation.
- [x] `--abort-on-critical` aborts only for an explicit critical abort recommendation.
- [x] MCP `ensemble_supervise` is advertised only when a supervisor runner is configured and delegates
  to the same policy.

**Verification:**

- [x] Unit tests for prompt assembly and parser.
- [x] Smoke with a mock or harmless supervisor prompt.
- [ ] Manual test during a live `ensemble run --watch <name>`.

**Dependencies:** Tasks 2-3.

**Files likely touched:**

- `src/supervisor.rs`
- `src/main.rs`
- `src/mcp.rs`
- `src/lib.rs`

**Estimated scope:** Medium.

### Task 7: Extend single-machine smoke

**Description:** Expand the existing PowerShell smoke into repeatable local team checks.

**Acceptance criteria:**

- [x] Existing `pwsh scripts/smoke.ps1` still passes.
- [x] New smoke can run `codex + claude + agy` with bounded timeouts and records the bounded failure
  when a required reviewer flakes.
- [x] New smoke can run a dry-run or non-mutating launcher check for `codex`, `claude`, `opencode`, and
  `agy`.
- [x] Smoke records stream, board, and final git state.

**Verification:**

- [x] `pwsh -NoProfile -File scripts\smoke.ps1 -PreflightOnly -SmokeRoot D:\tmp\ensemble-smoke-task7-preflight3 -TargetDir D:\tmp\ensemble-smoke-task7-target -AgyTimeoutSecs 1`
- [x] `pwsh -NoProfile -File scripts\smoke.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-smoke-task7-full2 -TargetDir D:\tmp\ensemble-smoke-task7-target -TimeoutSecs 180 -AgyTimeoutSecs 5`
- [x] `pwsh -NoProfile -File scripts\smoke.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-smoke-task7-agy-review-240b -TargetDir D:\tmp\ensemble-smoke-task7-target -Reviewers claude,agy -TimeoutSecs 240 -AgyTimeoutSecs 5 -SkipSupervisor` ran bounded and recorded artifacts; it exited 1 because `review2` / local `agy` produced empty output, so quorum stayed 1/2.
- [x] `pwsh -NoProfile -File scripts\acceptance-single-machine.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-acceptance-auto -TargetDir D:\tmp\ensemble-target-agy-interactive-release -AgyTimeoutSecs 1`
- [x] `pwsh -NoProfile -File scripts\acceptance-single-machine.ps1 -NoBuild -SmokeRoot D:\tmp\ensemble-acceptance-controlled -TargetDir D:\tmp\ensemble-target-controlled-release -AgyTimeoutSecs 1`
- [x] New team launcher smoke command documented in the script header.

**Dependencies:** Tasks 4-6.

**Files likely touched:**

- `scripts/smoke.ps1`
- `AGENTS.md`

**Estimated scope:** Small to Medium.

### Task 8: Operator acceptance runbook

**Description:** Write the exact steps the operator runs when Phase 1 is ready for personal testing.

**Acceptance criteria:**

- [x] Runbook starts from a scratch repo.
- [x] It covers all four launchers.
- [x] It covers team message exchange.
- [x] It covers live watch, steer, abort, and supervisor advisory mode.
- [x] It includes pass/fail observations and cleanup.

**Verification:**

- [x] The runbook is followed once by the developer before asking the operator to test.
- [x] The operator can follow it without needing implementation context.

**Dependencies:** Tasks 1-7.

**Files likely touched:**

- `docs/RUNBOOK-single-machine-team-cli.md`
- `AGENTS.md`

**Estimated scope:** Small.

### Task 9: User install/uninstall readiness

**Description:** Make the development package usable from a normal terminal without typing a repo-local
binary path, and make cleanup remove the installed binary/PATH entry without deleting repo state.

**Acceptance criteria:**

- [x] `ensemble` resolves from a fresh PowerShell environment after install.
- [x] Install copies the built `ensemble.exe` to a stable user-level install directory.
- [x] Install updates User PATH idempotently.
- [x] Uninstall removes the User PATH entry and installed binary directory.
- [x] Uninstall can optionally remove only ensemble's MCP server entry from Codex, Claude, and opencode
  configs for a selected repo.
- [x] Uninstall does not delete repo-local `.ensemble/` state.

**Verification:**

- [x] `cargo check --target-dir D:\tmp\ensemble-target-install-task`
- [x] `cargo test --lib mcp_install --target-dir D:\tmp\ensemble-target-install-task`
- [x] `cargo test --bin ensemble --target-dir D:\tmp\ensemble-target-install-task-bin`
- [x] PowerShell parser checks for `scripts\install.ps1` and `scripts\uninstall.ps1`
- [x] Scratch MCP install/uninstall for `codex`, `claude`, and `opencode`
- [x] Scratch script install/uninstall with `-NoPath`
- [x] Real user install to `%LOCALAPPDATA%\ensemble\bin` and User PATH verification
- [x] `D:\Projects\mix_swarm` dry-run for `team status`, `codex`, `claude`, `opencode`, and `agy`

**Dependencies:** Tasks 1-8.

**Files likely touched:**

- `src/main.rs`
- `src/mcp_install.rs`
- `scripts/install.ps1`
- `scripts/uninstall.ps1`
- `docs/RUNBOOK-single-machine-team-cli.md`
- `AGENTS.md`

**Estimated scope:** Small to Medium.

## Checkpoints

### Checkpoint A: Team State Works

After Tasks 1-2:

- [x] `cargo test --lib team`
- [x] `ensemble team say "hello"` writes to board.
- [x] `ensemble team inbox --json` reads it back.
- [x] No vendor CLI required.

### Checkpoint B: Active CLI Can Control Team

After Task 3:

- [x] MCP stdio smoke can call `ensemble_team_say`.
- [x] MCP stdio smoke can call `ensemble_steer` and `ensemble_abort`.
- [x] Existing MCP tools still pass.

### Checkpoint C: Member Launchers Work

After Tasks 4-5:

- [x] `ensemble --print-config codex` resolves member/repo/team correctly.
- [x] `ensemble --print-config claude` resolves member/repo/team correctly.
- [x] `ensemble --print-config opencode` resolves member/repo/team correctly.
- [x] `--confirm-policy` is visible in launcher dry-runs and fails fast for unsupported opencode
  approve/deny policies.
- [x] `ensemble agy --prompt <text>` posts or visibly flakes without hanging.
- [x] Default interactive launchers use the controlled PTY path, so `steer` and `abort` target
  ensemble-launched members rather than only governed runs.

### Checkpoint D: Supervisor Works

After Task 6:

- [x] `ensemble supervise <name> --agent claude --json` has a parseable JSON output contract.
- [x] Advisory mode never mutates control feeds.
- [x] `--apply-steer` and `--abort-on-critical` mutate only when the parsed recommendation allows it.
- [ ] Operator live run confirms the recommendation is useful during `ensemble run --watch <name>`.

### Checkpoint E: Phase-1 Candidate

After Tasks 7-8:

- [ ] `cargo check --target-dir D:\tmp\ensemble-target-check` passes.
- [ ] WSL `CARGO_TARGET_DIR=$HOME/ensemble-target cargo test` passes.
- [ ] WSL `cargo clippy --all-targets -- -D warnings` passes.
- [x] `pwsh scripts\acceptance-single-machine.ps1 -NoBuild -TargetDir D:\tmp\ensemble-target-agy-interactive-release -SmokeRoot D:\tmp\ensemble-acceptance-auto` passes.
- [ ] `pwsh scripts/smoke.ps1` passes.
- [x] `pwsh scripts/smoke.ps1 -Reviewers claude,agy -TimeoutSecs 240` passes, or documents exactly which
  CLI flaked and why.
- [x] Operator runbook exists and was dry-run by the developer.

## Developer Test Flow

1. Start from current repo.

   ```powershell
   git status --short
   cargo check --target-dir D:\tmp\ensemble-target-check
   ```

2. Build and run hermetic tests after each task.

   ```powershell
   cargo test --lib --target-dir D:\tmp\ensemble-target-check
   ```

   Preferred full verification on WSL:

   ```bash
   cd /mnt/d/Projects/ensemble
   CARGO_TARGET_DIR=$HOME/ensemble-target cargo test
   CARGO_TARGET_DIR=$HOME/ensemble-target cargo clippy --all-targets -- -D warnings
   ```

3. Run local smoke after launcher/control work lands.

   ```powershell
   pwsh scripts/smoke.ps1 -SmokeRoot D:\tmp\ensemble-smoke-single-machine -TimeoutSecs 180
   pwsh scripts/smoke.ps1 -SmokeRoot D:\tmp\ensemble-smoke-single-machine -Reviewers claude,agy -TimeoutSecs 240
   ```

4. Run MCP stdio smoke for new tools.

   Minimal sequence:

   - `initialize`
   - `tools/list`
   - `ensemble_team_say`
   - `ensemble_team_inbox`
   - `ensemble_steer`
   - `ensemble_abort`
   - existing `ensemble_board_post/read`

5. Run a scratch-repo manual developer pass.

   ```powershell
   $r = "D:\tmp\ensemble-team-dev"
   Remove-Item -Recurse -Force $r -ErrorAction SilentlyContinue
   New-Item -ItemType Directory $r | Out-Null
   Set-Location $r
   git init -q
   git config user.email "team-dev@example.invalid"
   git config user.name "ensemble team dev"
   "# team dev" | Set-Content README.md -Encoding ascii
   git add README.md
   git commit -q -m init
   git branch -M main
   ```

   Then:

   ```powershell
   ensemble team say "operator online"
   ensemble team inbox --json
   ensemble --print-config codex
   ensemble --print-config claude
   ensemble --print-config opencode
   ensemble --timeout 30 --print-prompt agy --prompt "Summarize the team board."
   ```

## Operator Personal Test Flow

These are the steps to hand to the operator when Phase 1 is marked ready.

### Setup

1. Open a fresh terminal.

   ```powershell
   cd D:\tmp
   mkdir ensemble-team-acceptance
   cd ensemble-team-acceptance
   git init -q
   git config user.email "operator@example.invalid"
   git config user.name "operator"
   "# ensemble team acceptance" | Set-Content README.md -Encoding ascii
   git add README.md
   git commit -q -m init
   git branch -M main
   ```

2. Confirm local CLIs.

   ```powershell
   ensemble doctor
   ensemble team status
   ```

   Pass when `codex`, `claude`, and at least one of `agy` or `opencode` are visible. If a CLI is missing,
   the test can continue but the missing CLI must be recorded.

### Test 1: Shared Team Board

```powershell
ensemble team say "operator: acceptance test started"
ensemble team inbox --json
```

Pass when the message is present and attributed to `operator`.

### Test 2: Launch Team Members

Open separate terminals:

```powershell
ensemble codex
ensemble claude --continue
ensemble opencode --continue
ensemble agy --continue
```

In MCP-capable CLIs, ask:

```text
Use the ensemble tools. Read the team inbox, then post a short hello message with your member name.
```

For `agy`, ask it to summarize the recent team board if interactive; if the wrapper is one-shot, run the
documented `ensemble agy --prompt <text>` team command.

Pass when:

- `ensemble team inbox` shows posts from at least two distinct AI members.
- Missing/flaked members are visible as flake messages, not silent failures.

### Test 3: Governed Run With Watch

In terminal A:

```powershell
ensemble run "Create RESULT.txt with exactly one line: TEAM_PHASE1_OK" --watch team-phase1 --merge --no-discover
```

In terminal B:

```powershell
ensemble watch team-phase1 --follow
```

Pass when terminal B shows implementer result, test result if configured, reviewer verdict, and conductor
decision.

### Test 4: Steer

Start a run with at least two rounds available. While it is running:

```powershell
ensemble steer team-phase1 "Stay focused: only edit RESULT.txt and do not touch README.md"
```

Pass when the watch stream shows the steer/injected event and the next round reflects the instruction.

### Test 5: Abort

Start a deliberately long or harmless run:

```powershell
ensemble run "Wait for operator instruction before changing files" --watch abort-demo --no-discover
```

Then:

```powershell
ensemble abort abort-demo
```

Repeat with:

```powershell
ensemble abort abort-demo --hard
```

Pass when clean abort stops at a round boundary and hard abort stops the running CLI promptly.

### Test 6: Supervisor Advisory

During or after a watched run:

```powershell
ensemble supervise team-phase1 --agent claude --json
```

Pass when the result is structured and says one of:

- `on_track`
- `steer`
- `abort`
- `needs_human`

If the result is `steer`, test:

```powershell
ensemble supervise team-phase1 --agent claude --apply-steer
```

Pass when a steer command is appended and visible in `ensemble watch`.

### Test 7: Final Phase-1 Acceptance

Run:

```powershell
git status --short
git log --oneline --decorate --all -5
ensemble team status --json
```

Phase 1 passes operator acceptance when:

- At least two AI CLIs post to the same team board.
- The governed run lands or escalates transparently with a visible reason.
- `watch` shows real progress.
- `steer` changes a later prompt or is visibly queued.
- `abort` stops a run.
- `supervise` produces a structured recommendation.
- A missing or flaky `agy`/`opencode` does not hang the team.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| `opencode` hangs headlessly | High | Use per-agent timeout, `on_flake = "exclude"`, and never make it required for first smoke. |
| `agy` cannot act as MCP client | Medium | Treat `agy` as direct UI plus bounded adapter wrapper in Phase 1; use `agy --prompt` to post final output to board. |
| Vendor config mutation surprises user | High | Provide dry-run/print mode and reuse idempotent `mcp_install` renderer. |
| Supervisor prompt becomes too large | Medium | Use bounded stream/board/diff excerpts with explicit truncation. |
| Control commands apply to wrong run | High | Use sanitized watch/team names and show resolved repo/name before appending. |
| Shared board grows without bound | Low | Phase 1 uses cursors and bounded reads; compaction can be later. |

## Completion Definition

The implementation can be called Phase 1 complete only after:

- All task acceptance criteria above are checked.
- Developer test flow passes.
- Operator personal test flow is documented and at least once dry-run by the developer.
- The operator personally runs the acceptance flow and confirms it matches the intended workflow.
- Any unavailable local CLI is recorded with exact command, exit status, and output excerpt.
