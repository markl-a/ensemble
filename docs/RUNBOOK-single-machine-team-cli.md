# Single-Machine Team CLI Runbook

This runbook records the expected user workflow for the Phase 1 `ensemble` package:
one operator, one repo, one local machine, and several AI CLIs connected to the same
repo-local team state.

## User Model

The primary user is an operator working in a normal terminal. They usually keep one
AI CLI as the main working surface, for example Codex or Claude Code, and use
`ensemble` to connect other local AI CLIs to the same team board and live control
feeds.

The package should feel like this:

```powershell
ensemble codex
ensemble claude --continue
ensemble agy --continue
ensemble opencode --continue
```

Each launched member joins the current repo's `.ensemble/` state. MCP-capable CLIs
can call team/control tools directly. The `agy` path supports direct interactive UI
launching through ensemble's controlled terminal driver; add `agy --prompt` /
`agy -p` only when you want a bounded one-shot team turn that posts a result or
visible flake to the board.

## Mental Model

`ensemble` has three local planes:

- Team board: durable repo-local messages under `.ensemble/`, used for status,
  coordination, results, and visible flakes.
- Live streams: watched run output under `.ensemble/stream/`, used by `watch` and
  supervisor checks.
- Control feeds: intervention files under `.ensemble/control/`, used by `steer`
  and `abort` for both governed runs and ensemble-launched interactive CLIs.

The operator should not need to understand those files during normal use. The useful
surface is:

- Start members with `ensemble [ensemble options] codex|claude|opencode|agy [vendor args...]`.
- Inspect shared state with `ensemble team status`, `ensemble team inbox`, and
  `ensemble watch`.
- Let MCP-capable CLIs call `ensemble_team_status`, `ensemble_team_say`,
  `ensemble_team_inbox`, `ensemble_watch`, `ensemble_supervise`, `ensemble_steer`,
  and `ensemble_abort`.
- Ask a supervisor check to detect drift before mutating control feeds.
- Steer an ensemble-launched member in real time with `ensemble steer <member>
  "<prompt>"`; the controlled launcher sends Escape, waits briefly, then types
  the corrective prompt and Enter into that CLI.

Launcher syntax is intentionally split at the AI CLI name:

```powershell
ensemble [ensemble options] <codex|claude|opencode|agy> [that CLI's own args]
```

Examples:

```powershell
ensemble --repo . --team phase1 claude --continue
ensemble --repo . --team phase1 codex resume --last
ensemble --repo . --team phase1 --confirm-policy approve --timeout 30 agy --prompt "Summarize the team board." --continue
```

The old `--` separator form is removed. Put ensemble options before the AI CLI
name; put vendor options after it.

## Quick Start

Install the development build for the current Windows user:

```powershell
cd D:\Projects\ensemble
pwsh scripts\install.ps1
```

Open a new terminal after installation so PowerShell picks up the updated User
PATH. The installed command should resolve from any repo:

```powershell
Get-Command ensemble
ensemble doctor
```

Start in the repo the team should work on:

```powershell
ensemble doctor
ensemble team status --repo . --team phase1
```

Preview a member launcher before mutating vendor MCP config:

```powershell
ensemble --repo . --team phase1 --member codex@local --confirm-policy ask --print-config codex
ensemble --repo . --team phase1 --member claude@local --confirm-policy ask --print-config claude
ensemble --repo . --team phase1 --member opencode@local --confirm-policy ask --print-config opencode
```

Launch the members in separate terminals:

```powershell
ensemble --repo . --team phase1 --member codex@local --confirm-policy ask codex
ensemble --repo . --team phase1 --member claude@local --confirm-policy ask claude --continue
ensemble --repo . --team phase1 --member opencode@local --confirm-policy ask opencode --continue
ensemble --repo . --team phase1 --member agy@local --confirm-policy ask agy
```

Each of these launchers is controlled by default. From another terminal, the
operator or a managing MCP-capable CLI can intervene:

```powershell
ensemble steer claude@local "Stop the current approach. Re-read the team inbox and focus only on RESULT.txt." --repo .
ensemble abort claude@local --repo .
ensemble abort claude@local --hard --repo .
```

Clean `abort` sends Escape. `--hard` sends Escape and kills the launched child
process. This only applies to sessions started through `ensemble`; a vendor CLI
opened directly cannot be interrupted by ensemble.

Run a bounded agy team turn only when you want a non-interactive board post:

```powershell
ensemble --repo . --team phase1 --member agy@local --timeout 30 --confirm-policy ask --json agy --prompt "Read the team board and summarize current blockers."
```

Read the shared board:

```powershell
ensemble team inbox --repo . --team phase1 --json
```

Uninstall the development build:

```powershell
pwsh D:\Projects\ensemble\scripts\uninstall.ps1
```

To also remove ensemble's MCP server entry from local CLI configs for one repo:

```powershell
pwsh D:\Projects\ensemble\scripts\uninstall.ps1 -RemoveMcpConfig -Repo D:\Projects\mix_swarm -Clients codex,claude,opencode
```

The uninstall script removes the installed binary directory and the User PATH
entry. It does not delete repo-local `.ensemble/` state.

## Main Operator Workflow

The most likely daily workflow is:

1. The operator opens the repo and starts the primary AI CLI through `ensemble`,
   usually `ensemble codex` or `ensemble claude --continue`.
2. The operator starts one or two extra members in separate terminals, or invokes
   `ensemble agy` as an interactive Antigravity terminal.
3. The operator asks the primary MCP-capable CLI to read the team inbox, post status,
   inspect a watched run, or call a supervisor tool.
4. The operator runs governed work with `ensemble run --watch <name>` when they want
   implement/review/test structure.
5. The operator watches progress with `ensemble watch <name> --follow`.
6. If a member drifts, the operator uses advisory supervision first, then applies
   `steer` or `abort` to the watched run or ensemble-launched member only when
   the recommendation and their own judgment agree.

## MCP-Capable CLI Usage

Inside Codex, Claude Code, or opencode after launching through `ensemble`, the user can
ask the CLI to use its MCP tools:

```text
Use the ensemble tools. Read the team inbox, summarize current team state, then post
a short status message with your member name.
```

Expected tool path:

- `ensemble_team_status` to inspect the current team.
- `ensemble_team_inbox` to read recent board messages.
- `ensemble_team_say` to post a short coordination message.
- `ensemble_watch` to inspect a live run stream.
- `ensemble_supervise` to ask a local supervisor agent for a drift recommendation.
- `ensemble_steer` or `ensemble_abort` only after an explicit user decision.

## Supervisor Workflow

Use advisory mode first:

```powershell
ensemble supervise team-phase1 --repo . --team phase1 --agent claude --json
```

If the parsed recommendation is `steer` and the operator wants to apply it:

```powershell
ensemble supervise team-phase1 --repo . --team phase1 --agent claude --apply-steer
```

If the parsed recommendation is `abort`, it must also mark `critical=true` before
`--abort-on-critical` mutates the control feed:

```powershell
ensemble supervise team-phase1 --repo . --team phase1 --agent claude --abort-on-critical
```

Default supervision is intentionally advisory. A supervisor result should make the
operator faster, not silently override the team.

## Governed Run Workflow

Start a governed run with a visible stream:

```powershell
ensemble run "Create RESULT.txt with exactly one line: TEAM_PHASE1_OK" --watch team-phase1 --merge --no-discover
```

Watch from another terminal:

```powershell
ensemble watch team-phase1 --follow
```

Steer while the run is active:

```powershell
ensemble steer team-phase1 "Stay focused: only edit RESULT.txt and do not touch README.md"
```

Abort a bad or stuck run:

```powershell
ensemble abort team-phase1
ensemble abort team-phase1 --hard
```

## Confirmation Policy

The default policy is `ask`. This keeps vendor confirmation choices visible to the
operator.

Use `approve` only in a trusted scratch repo or controlled smoke:

```powershell
ensemble --repo . --team phase1 --confirm-policy approve codex
ensemble --repo . --team phase1 --confirm-policy approve claude --continue
ensemble --repo . --team phase1 --confirm-policy approve --timeout 30 agy --prompt "Summarize the team board." --continue
```

Use `deny` when the user wants defensive read-only behavior where supported:

```powershell
ensemble --repo . --team phase1 --confirm-policy deny codex
ensemble --repo . --team phase1 --confirm-policy deny claude --continue
```

On the current local opencode build, `approve` and `deny` are rejected because
`opencode --help` does not expose a stable confirmation flag. This is intentional:
`ensemble` should fail visibly instead of pretending it can operate hidden prompts.

## Operator Acceptance Flow

Before opening real AI CLI terminals, run the automated local acceptance. It proves
the repeatable parts of Phase 1 without depending on a live UI to click through
prompts:

```powershell
cd D:\Projects\ensemble
pwsh scripts\acceptance-single-machine.ps1
```

For a faster run after a release binary already exists:

```powershell
pwsh scripts\acceptance-single-machine.ps1 -NoBuild -TargetDir D:\tmp\ensemble-target-agy-interactive-release -SmokeRoot D:\tmp\ensemble-acceptance-auto
```

This checks `doctor`, team board read/write, auto member names, launcher MCP config
previews, a controlled fake `codex` launcher, controlled `agy` launch via
`agy --help`, bounded `agy --prompt` result/flake visibility, real `ensemble mcp`
stdio tools, `watch`, `steer`, and `abort`. It does not prove that a vendor UI
visibly exposes MCP tools; that still requires the manual terminal pass below.

If Phantom Mesh is installed on the same machine, run the bridge check before
moving to Phase 2. It verifies the path Phantom -> shell tool -> ensemble agent
`--node local` -> local AI CLI:

```powershell
pwsh scripts\phantom-single-machine.ps1 -Repo D:\Projects\ensemble -TargetDir D:\tmp\ensemble-acceptance-phase1-target -NoBuild
```

The verifier requires both the direct ensemble call and the Phantom shell call to
return JSON with `ok:true` and `node:"local"`. It uses a temporary `.cmd` shim
because the current `phantom tool shell` command is sensitive to nested quotes.
For this bridge check, keep `-Repo`, the temporary shim path, and `-Prompt`
limited to simple shell-safe characters (`A-Z`, `a-z`, `0-9`, `_`, `.`, `/`,
`:`, `@`, `\`, `=`, `-`); the script fails clearly if a value cannot be passed
through that simple shell bridge.

When Phase 1 manual testing and the Phantom bridge look good, run the local
readiness wrapper before moving to the five-machine Phase 2 pass:

```powershell
pwsh scripts\phase2-local-ready.ps1 -Repo D:\Projects\ensemble
```

It chains the deterministic Phase 1 acceptance, Phantom bridge, and the local
Phase 2 slices that do not require m1~m5 to be online. Add `-RunCleanReinstall`
only when you intentionally want to exercise the install/uninstall path.

Use a scratch repo for the first personal test:

```powershell
cd D:\tmp
mkdir ensemble-team-acceptance
cd ensemble-team-acceptance
git init -q
git config user.email "operator@example.invalid"
git config user.name "operator"
git config core.autocrlf false
@'
.ensemble/
crew.toml
.mcp.json
opencode.json
'@ | Set-Content .gitignore -Encoding ascii
"# ensemble team acceptance" | Set-Content README.md -Encoding ascii
@'
pipeline = ["implement", "review"]

[gate]
min_approvals = 1
max_rounds = 2
on_flake = "exclude"
stall_limit = 2
max_task_secs = 360

[test]
command = "findstr /C:TEAM_PHASE1_OK RESULT.txt"

[roles.implement]
agent = "codex"

[roles.review]
agent = "claude"
blind = true

[agents.codex]
timeout = 180

[agents.claude]
timeout = 180
'@ | Set-Content crew.toml -Encoding ascii
git add .gitignore README.md
git commit -q -m init
git branch -M main
```

Acceptance checks:

1. `ensemble doctor` shows the available local CLIs.
2. `ensemble team say "operator: acceptance test started"` appears in
   `ensemble team inbox --json`.
3. At least two members launched through `ensemble codex|claude|opencode|agy` post
   to the same team board, or a missing/flaky member writes a visible `flake`.
4. `ensemble run --watch <name>` produces a stream visible through
   `ensemble watch <name> --follow`.
5. `ensemble steer <name> "<prompt>"` is visible in the run and affects a later
   prompt or is visibly queued.
6. `ensemble abort <name>` stops a harmless test run.
7. `ensemble steer <member> "<prompt>"` affects an interactive member launched
   through `ensemble <ai-cli>` by interrupting and injecting the correction, or
   the limitation is recorded with the exact CLI/output.
8. `ensemble supervise <name> --agent claude --json` returns one of
   `on_track`, `steer`, `abort`, or `needs_human`.

Cleanup is normal git/worktree cleanup for the scratch repo. Do not reuse a scratch
repo that has a failed hard-abort test unless the final `git status --short` is
understood.

## Developer Dry-Run Status

The automated part of this runbook was dry-run on 2026-06-22 in a scratch repo.

Passed:

- User-level install to `%LOCALAPPDATA%\ensemble\bin`, User PATH verification,
  and scratch uninstall script cleanup.
- Scratch MCP install/uninstall for Codex, Claude, and opencode using isolated
  config paths; only the `ensemble` MCP entry was removed.
- Installed command resolution from a fresh shell environment and `ensemble doctor`.
- `D:\Projects\mix_swarm` dry-run for `team status`, `--print-config codex`,
  `--print-config claude`, `--print-config opencode`, and `--print-prompt agy`.
- Scratch repo setup, `ensemble doctor`, `team status`, `team say`, and `team inbox`.
- Non-mutating launcher previews for `codex`, `claude`, and `opencode`.
- Bounded `ensemble agy --prompt` turn. On this machine it wrote a visible
  `flake` instead of hanging.
- Raw MCP stdio posts from two member identities to the same team board.
- Governed `ensemble run --watch team-phase1 --merge --no-discover` with the
  minimal `crew.toml` above. Codex implemented, the test gate passed, Claude
  returned `VERDICT: LGTM`, and the run landed.
- `ensemble supervise team-phase1 --agent claude --json` returned `on_track`.
- `ensemble steer` and `ensemble abort --hard` appended the expected control-feed
  commands.
- A unit-level controlled PTY test verified child-process launch and exit-code
  propagation, and the automated acceptance script now verifies the default
  `ensemble codex` launcher path through an isolated fake Codex shim.
- `pwsh -NoProfile -File scripts\acceptance-single-machine.ps1 -NoBuild -SmokeRoot
  D:\tmp\ensemble-acceptance-controlled -TargetDir D:\tmp\ensemble-target-controlled-release
  -AgyTimeoutSecs 1` passed with the controlled fake Codex launcher and controlled
  agy `--help` launch.

Manual-only:

- Actually launching `ensemble codex`, `ensemble claude`, and `ensemble opencode`
  in separate interactive terminals and asking them to use MCP tools.
- Proving `steer` and `abort --hard` affect currently running real vendor CLIs in
  real time. The dry-run verifies control-feed writes and the controlled launcher
  path; the operator acceptance pass must verify each vendor's live interruption
  behavior.

## Expected Usage Scenarios

Primary CLI plus watcher:

- The user works mainly in Codex or Claude Code.
- The same CLI calls `ensemble_supervise` to check whether another run is drifting.
- The user applies `steer` or `abort` only after seeing the recommendation.

Local council:

- The user starts multiple CLIs under the same team.
- Each member posts short findings to the board.
- The operator compares answers through `ensemble team inbox`.

Antigravity helper:

- The user calls `ensemble --repo <path> --team <name> agy [agy args...]` to open
  the Antigravity UI in the target repo.
- The terminal is controlled by ensemble, so `steer` and `abort` can target the
  running member while interactive choices, confirmations, approval, and denial
  remain visible to the operator.

Bounded helper:

- The user calls `ensemble --timeout <secs> agy --prompt <text> [agy args...]`.
- Success is posted as `result`; timeout or tool failure is posted as `flake`.
- The team keeps moving even if agy cannot complete.

Governed implementation:

- The user starts `ensemble run --watch <name>`.
- Implementer, test gate, and reviewer results land in the stream.
- Supervisor checks and control feeds are available when the run needs intervention.

## Current Limits

- Full PTY/expect automation for operating arbitrary vendor confirmation menus is
  not implemented yet. The default controlled launcher can send Escape plus an
  injected prompt, but it cannot guarantee every vendor menu or in-flight model
  state will respond identically.
- Phase 1 does not inspect hidden vendor subagents unless their activity appears in
  observable MCP calls, logs, tool output, or streams.
- Cross-machine team control is Phase 2/fleet work.
- Supervisor quality depends on the selected local supervisor CLI being installed,
  authenticated, and able to return the requested strict JSON.
- Operator live acceptance still has to be run before declaring Phase 1 complete.
