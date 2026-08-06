# Phase 1 + Phase 2 Manual Test Runbook

This runbook is for operator-driven validation of `ensemble` after local changes or before using it on real projects.
It covers:

- Phase 1: one machine, one repo, multiple local AI CLIs.
- Phase 2: multiple machines connected through the tailnet, with one conductor and satellite projects.
- Install, uninstall, service startup, mesh discovery, and cross-machine run evidence.

Use placeholders such as `<repo>`, `<team>`, `<m1>`, and `<service-port>` for private machine names, paths, and ports. Do not commit `phase2-fleet.local.json` or secrets.

## 0. Prerequisites

On every machine:

- Git is installed and the target project repo exists locally.
- Rust/Cargo is available if building from source.
- Tailscale is connected and peer names resolve.
- The intended AI CLIs are installed and authenticated:
  - Phase 1: `codex`, `claude`, `opencode`, `agy` if available.
  - Phase 2 main project: usually `codex`, `claude`, `agy` in governed roles; `opencode` can be launched interactively but is not a reliable headless reviewer yet.
  - Phase 2 satellites: `codex` + `claude` are the minimal two-vendor governed pair.

Quick readiness:

```powershell
ensemble doctor
```

Expected: the local CLIs you intend to use show `[ok]`, `tailscale` is `[ok]`, and `git-repo` is `[ok]` when run inside a repo.

## 1. Install / Uninstall

### Windows

From the `ensemble` repo:

```powershell
cd <ensemble-repo>
pwsh scripts\install.ps1
```

Open a new terminal after install, then verify:

```powershell
Get-Command ensemble
ensemble doctor
```

Uninstall:

```powershell
pwsh <ensemble-repo>\scripts\uninstall.ps1
```

Optional MCP cleanup for a target repo:

```powershell
pwsh <ensemble-repo>\scripts\uninstall.ps1 `
  -RemoveMcpConfig `
  -Repo <repo> `
  -Clients codex,claude,opencode
```

### macOS / Linux

From the `ensemble` repo:

```bash
git pull --ff-only
cargo build --release --bin ensemble
cargo install --path . --force
ensemble doctor
```

If `ensemble` is not found, add Cargo bin to your shell PATH:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

For a one-off test without installing:

```bash
./target/release/ensemble doctor
```

## 2. Phase 1 Manual Test: Single Machine Multi-AI CLI

Goal: prove one local repo can host multiple ensemble-launched AI CLIs, shared team state, MCP team/control tools, watch/steer/abort, supervisor checks, and governed smoke.

### 2.1 Deterministic Phase 1 Acceptance

Run from `<ensemble-repo>` on Windows:

```powershell
pwsh -NoProfile -File scripts\acceptance-single-machine.ps1 `
  -SmokeRoot <scratch-dir>\ensemble-phase1-acceptance `
  -TargetDir <scratch-dir>\ensemble-phase1-target `
  -AgyTimeoutSecs 1
```

Expected:

- `single-machine automated acceptance passed`
- team board works: `team status/say/inbox`
- launcher config previews work for `codex`, `claude`, `opencode`
- controlled launcher path works
- `agy` bounded timeout posts a visible `flake` if it cannot answer in time
- MCP team/control tools are listed and callable
- `watch`, `steer`, `abort` work

The `agy` one-second bounded path may return a visible flake. That is acceptable for this deterministic test if the script exits successfully.

### 2.2 Start Interactive Team Members

Open a target project:

```powershell
cd <repo>
ensemble team status --repo . --team phase1
```

Start each member in a separate terminal:

```powershell
ensemble --repo . --team phase1 --member codex@local --confirm-policy ask codex
ensemble --repo . --team phase1 --member claude@local --confirm-policy ask claude --continue
ensemble --repo . --team phase1 --member opencode@local --confirm-policy ask opencode
ensemble --repo . --team phase1 --member agy@local --confirm-policy ask agy
```

Syntax rule:

```text
ensemble [ensemble options] <codex|claude|opencode|agy> [vendor CLI args]
```

Examples:

```powershell
ensemble --repo . --team phase1 claude --continue
ensemble --repo . --team phase1 codex resume --last
ensemble --repo . --team phase1 --confirm-policy ask agy --continue
```

### 2.3 Shared Board and Intervention

From another terminal:

```powershell
ensemble team say --repo . --team phase1 "operator: phase1 manual test started"
ensemble team inbox --repo . --team phase1 --json
ensemble team status --repo . --team phase1 --json
```

Steer a controlled member:

```powershell
ensemble steer claude@local "Stop. Read the team inbox, summarize the current goal, then continue only with the requested file." --repo .
```

Abort a controlled member:

```powershell
ensemble abort claude@local --repo .
ensemble abort claude@local --hard --repo .
```

Expected:

- `steer` appends a control event and sends Escape plus the prompt to ensemble-launched sessions.
- clean `abort` sends Escape.
- `abort --hard` also kills the launched child process.
- This only works for sessions started through `ensemble`.

### 2.4 Supervisor Check

For a watched run/member named `<watch>`:

```powershell
ensemble supervise <watch> --repo . --team phase1 --agent claude --json
```

Apply control only when you explicitly want mutation:

```powershell
ensemble supervise <watch> --repo . --team phase1 --agent claude --apply-steer
ensemble supervise <watch> --repo . --team phase1 --agent claude --abort-on-critical
```

Expected: default supervision is advisory. It should post/read evidence without silently steering or aborting unless flags are set.

### 2.5 Single-Machine Governed Smoke

Run a real governed local smoke:

```powershell
pwsh -NoProfile -File scripts\smoke.ps1 `
  -Repo <ensemble-repo> `
  -SmokeRoot <scratch-dir>\ensemble-phase1-smoke `
  -TargetDir <scratch-dir>\ensemble-phase1-target `
  -TimeoutSecs 180 `
  -AgyTimeoutSecs 1
```

Expected:

- `single-machine multi-AI smoke passed`
- governed run reaches `LANDED` or an explicit accepted terminal state
- test gate runs
- reviewer verdict is visible
- watch transcript and team board contain the conductor decision
- supervisor advisory returns a sane status

## 3. Phase 2 Manual Test: Multi-Machine Install and Connection

Goal: prove multiple machines can serve agent capacity, discover one another, route team/watch/steer/abort across nodes, and run governed main/satellite work with repeatable evidence.

### 3.1 Choose the Service Port

Default is `7878`.

If another local service uses `7878` on your machines, set a fleet service port such as `8788` in the private manifest and pass it consistently:

```powershell
ensemble up --port <service-port>
ensemble mesh --port <service-port>
ensemble nodes --port <service-port>
```

### 3.2 Start Service on Every Machine

Foreground quick test on each machine:

```powershell
ensemble up --port <service-port>
```

Expected:

- It prints local CLIs.
- It lists reachable tailnet peers.
- It stays running until Ctrl-C.

Persistent service preview:

```powershell
ensemble serve --install-service --port <service-port> --print
ensemble serve --uninstall-service --print
```

Install persistent service:

```powershell
ensemble serve --install-service --port <service-port>
```

Remove persistent service:

```powershell
ensemble serve --uninstall-service
```

### 3.3 Verify Mesh from the Conductor

On `<m1>` conductor:

```powershell
ensemble mesh --port <service-port>
ensemble nodes --port <service-port>
```

Expected:

- `mesh` shows local CLIs and remote peers.
- `nodes` shows agent-to-host helper routes.
- The conductor itself is not listed as a remote peer. That is normal.

## 4. Phase 2 Fleet Manifest

Create an untracked `phase2-fleet.local.json` on the conductor. Do not commit it.

Minimal shape:

```json
{
  "nodes": ["<m1>", "<m2>", "<m3>", "<m4>", "<m5>"],
  "conductor": "<m1>",
  "service": { "port": 8788 },
  "main": {
    "repo": "<main-repo>",
    "team": "main",
    "watch": "main",
    "task": "phase2 main smoke run: make a tiny verifiable scratch change, then leave LANDED evidence",
    "test": "cargo test --quiet",
    "merge": true,
    "routes": {
      "codex": "<m1>",
      "claude": "<m1>",
      "agy": "<m2>"
    }
  },
  "satellites": [
    {
      "name": "sat-a",
      "repo": "<satellite-a-repo>",
      "node": "<m2>",
      "team": "sat-a",
      "watch": "sat-a",
      "task": "phase2 satellite A smoke run",
      "test": "cargo test --quiet",
      "routes": {
        "codex": "<m2>",
        "claude": "<m3>"
      }
    }
  ]
}
```

Notes:

- Main routes that point at the conductor are generated as local agents, without `node = ...`, to avoid remote git-sync back to the same machine.
- Satellite `node` is the project ownership/execution node.
- Satellite `routes.codex` and `routes.claude` may override where those CLIs actually run.
- All routes must point at `nodes`.
- Add `forbidden_nodes` if a tailnet peer must not be used.

Validate shape:

```powershell
pwsh -NoProfile -File scripts\phase2-goal-shape.ps1 -Manifest phase2-fleet.local.json
```

Preview generated plan:

```powershell
pwsh -NoProfile -File scripts\phase2-fleet.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node all `
  -PlanOnly
```

Materialize generated crew files:

```powershell
pwsh -NoProfile -File scripts\phase2-fleet.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node <this-node> `
  -Materialize `
  -PlanOnly
```

## 5. Phase 2 Slice A: Control Plane

On conductor:

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 `
  -Repo <ensemble-repo> `
  -FleetManifest phase2-fleet.local.json `
  -FleetNode all `
  -CheckFleetManifestNodes `
  -SkipSliceB `
  -SkipSliceD
```

Expected:

- local `team status --node local` works.
- `--node auto` fails explicitly.
- loopback remote `team/watch/steer/abort` works.
- wrong token fails with `Unauthorized`.
- `member@node` suffix routing works.
- mesh check excludes the conductor from expected remote peers.

Manual spot checks:

```powershell
ensemble team status --repo <repo> --team main --json --node local
ensemble watch <member>@<node> --repo <repo> --team main --since 0 --port <service-port>
ensemble steer <watch>@<node> "manual steer smoke" --repo <repo> --team main --port <service-port>
ensemble abort <watch>@<node> --repo <repo> --team main --port <service-port>
```

## 6. Phase 2 Slice B: Cross-Machine Governed Run

First inspect governance:

```powershell
ensemble crew inspect --crew <generated-crew.toml> --json
```

Expected:

- `min_approvals >= 2`
- non-empty test command
- at least two reviewer roles
- at least two distinct reviewer vendors
- for a real cross-machine test, at least one active agent has a remote `node`

Run:

```powershell
ensemble run "<task>" `
  --crew <generated-crew.toml> `
  --repo <repo> `
  --team <team> `
  --watch <watch>
```

Watch:

```powershell
ensemble watch <watch> --repo <repo> --team <team> --follow
```

Optional intervention:

```powershell
ensemble steer <watch> "Keep the change minimal and rerun tests." --repo <repo> --team <team>
ensemble abort <watch> --repo <repo> --team <team>
ensemble abort <watch> --hard --repo <repo> --team <team>
```

Verify evidence after the run:

```powershell
pwsh -NoProfile -File scripts\phase2-run-evidence.ps1 `
  -Repo <repo> `
  -Team <team> `
  -Watch <watch> `
  -ExpectTerminal landed
```

Expected:

- run reaches `LANDED` or explicit `ESCALATED`
- team board has conductor decision
- watch stream has conductor decision
- test gate output is present
- `LANDED` requires test pass and reviewer quorum

## 7. Phase 2 Slice C: Five-Machine Repeatable Fleet

On each selected node, run only that node's assigned project:

```powershell
pwsh -NoProfile -File scripts\phase2-fleet.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node <this-node> `
  -Materialize `
  -RunSelected `
  -VerifyEvidence `
  -RepeatCount 2
```

Expected:

- each selected project runs twice
- each repeat reaches `LANDED` or explicit accepted terminal state
- each repeat has evidence verification
- report is written:

```text
<repo>/.ensemble/phase2-fleet/acceptance-<project>-<node>.json
```

The report must include generated crew hash and project spec hash. This prevents stale reports from satisfying a later manifest.

From a machine that can read every project repo:

```powershell
pwsh -NoProfile -File scripts\phase2-fleet.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node all `
  -VerifyReports `
  -RepeatCount 2
```

Expected: all main + satellite reports verify.

If each machine can only read its own project repo, run:

```powershell
pwsh -NoProfile -File scripts\phase2-fleet.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node <this-node> `
  -VerifyReports `
  -RepeatCount 2
```

## 8. Phase 2 Slice D: Clean Reinstall

This mutates the user-level install and ends by uninstalling `ensemble`.

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 `
  -Repo <ensemble-repo> `
  -TargetDir <scratch-dir>\ensemble-phase2-manual-target `
  -SmokeRoot <scratch-dir>\ensemble-phase2-manual-slice-d `
  -FleetManifest phase2-fleet.local.json `
  -SkipSliceA `
  -SkipSliceB `
  -SkipSliceC
```

Expected:

- baseline uninstall succeeds or reports already absent
- install succeeds
- service install/uninstall dry-run succeeds
- governed smoke succeeds
- `ensemble up` starts or retries loopback if the tailnet bind is already in use
- `mesh` and `nodes` work after reinstall
- final uninstall succeeds

After this, reinstall if you want `ensemble` on PATH again:

```powershell
pwsh scripts\install.ps1
```

## 9. Phantom Bootstrap Fallback

Use this when a peer cannot be reached by SSH/Tailscale SSH but has Phantom admin shell available on the private network.

Secret rules:

- Prefer `PHANTOM_CLUSTER_SECRET`.
- Or use an untracked `-SecretFile`.
- Never commit or print the secret.

Probe:

```powershell
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -SecretFile <untracked-secret-file> `
  -Action probe
```

Start ensemble service through Phantom:

```powershell
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -SecretFile <untracked-secret-file> `
  -Action start
```

Windows peer missing `ensemble.exe`:

```powershell
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node <windows-peer> `
  -SecretFile <untracked-secret-file> `
  -Action start `
  -WindowsExePath <local-built-ensemble.exe>
```

macOS/Linux peer missing `ensemble`:

```powershell
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node <mac-or-linux-peer> `
  -SecretFile <untracked-secret-file> `
  -Action start `
  -MacRepoPath '$HOME/Projects/ensemble' `
  -Branch <branch>
```

## 10. Troubleshooting

`ensemble` command not found:

- Windows: open a new terminal after `scripts\install.ps1`.
- macOS/Linux: ensure `~/.cargo/bin` is on PATH.

Port already in use:

- Choose `service.port` in `phase2-fleet.local.json`, for example `8788`.
- Use `ensemble up --port <service-port>`.

`mesh` does not show the conductor:

- Expected. `mesh` lists local CLIs separately and remote tailnet peers separately.

Remote mutation says `Unauthorized`:

- Wrong or missing token. This is the expected safe failure for protected remote control mutation.

`agy` bounded prompt flakes:

- For deterministic Phase 1 tests with a very short timeout, visible flake is acceptable if the script passes.
- For real governed review, use a realistic timeout and verify `agy` is authenticated.

`opencode` hangs headless:

- Do not use it as a headless governed reviewer yet.
- It is fine for controlled interactive launch and MCP-capable manual participation when available.

Main project auto-merge refuses dirty worktree:

- Expected safety behavior. Clean or commit unrelated changes before using `--merge`.

## 11. Minimum Manual Pass Matrix

Phase 1 pass:

- `acceptance-single-machine.ps1` exits 0.
- `smoke.ps1` exits 0.
- Manual `ensemble codex` / `ensemble claude --continue` / `ensemble agy` launch works.
- `team inbox`, `watch`, `steer`, `abort`, and `supervise` work.

Phase 2 pass:

- Every node can run `ensemble up --port <service-port>`.
- Conductor `mesh` shows the four remote peers.
- `phase2-goal-shape.ps1` passes.
- Slice A control checks pass.
- At least one governed cross-machine run reaches a clear terminal state.
- `phase2-fleet.ps1 -RunSelected -VerifyEvidence -RepeatCount 2` passes for main and satellites.
- `phase2-fleet.ps1 -Node all -VerifyReports -RepeatCount 2` passes from a machine that can read all reports.
- Slice D clean reinstall passes.
