# ensemble — Repository Index

> Single entry point to **every folder and file** in this repo, plus a
> **one-function = one-file = one-owner** map for `src/`. Use this to find where a thing
> lives *before* writing new code, so the same functionality is never implemented twice.
>
> Maintenance rule: when you add/rename/delete a tracked file, update the matching row here.
> Last synced: 2026-06-28 (97 tracked files).

## Repo map (top level)

| Path | What lives here |
|---|---|
| [`src/`](#src--rust-source-one-responsibility-per-file) | Rust source — the single binary `ensemble`. One responsibility per file. |
| [`tests/`](#tests--integration--e2e) | Integration / end-to-end tests (cargo `tests/`). |
| [`docs/`](#docs--design-plans-specs-runbooks-adrs) | Design docs, phased plans, specs, runbooks, ADRs. |
| [`scripts/`](#scripts--powershell-opsverify) | PowerShell install / verify / fleet ops scripts. |
| [`examples/`](#examples--sample-crew-configs) | Sample `crew.toml` / fleet configs. |
| [`.github/`](#github--ci) | CI workflow. |
| [root files](#root-files) | `Cargo.toml`, `README.md`, crew TOMLs, license/notice, this index. |

---

## `src/` — Rust source (one responsibility per file)

Each row is the **single owner** of that responsibility. If you need that behavior, call into
this file — do not re-implement it elsewhere. See [Known duplication](#known-duplication--single-source-of-truth-debt) for the spots that currently violate this.

### Orchestration core
| File | Single responsibility | Key public types |
|---|---|---|
| `main.rs` | CLI entry point + every subcommand handler (argument parsing → library calls). **Oversized (~5k lines)** — see dup #2/#3. | (CLI driver) |
| `lib.rs` | Module declarations + public API re-exports. | — |
| `conductor.rs` | Drives one task: agents → gate → worktree land. | `Conductor`, `Decision`, `RunOutcome` |
| `crew.rs` | Crew config model (pipeline, gate, roles, agents, test). | `CrewConfig`, `GatePolicy`, `RoleConfig`, `AgentConfig`, `TestConfig` |
| `gate.rs` | Quorum arbitration (Approve / Changes / Escalate). | `RoleVerdict`, `GateDecision` |
| `verdict.rs` | Parse a reviewer's verdict text (LGTM/APPROVE vs CHANGES). | `Verdict` |
| `test_gate.rs` | Automated test firewall (exit 0 = green). | `TestOutcome` |
| `dispatch.rs` | Durable, resumable batch dispatch (ledger-driven). | `task_id()`, `run()` |

### Agents / adapters
| File | Single responsibility | Key public types |
|---|---|---|
| `adapter.rs` | Adapter trait + error / rate-limit detection. | `Adapter`, `AgentOutput`, `AdapterError`, `RateLimitInfo` |
| `exec_adapter.rs` | exec-style adapter (codex / claude / opencode). | `ExecAdapter` |
| `agy_adapter.rs` | PTY-based adapter for Google Antigravity (`agy`). | `AgyAdapter` |
| `remote_adapter.rs` | HTTP adapter to a remote node (git-bundle sync). | `RemoteAdapter` |
| `controlled.rs` | PTY control script (steer-key injection, abort). | `ControlledPtyConfig`, `ControlScript`, `PtyProgram` |

### Persistence / messaging
| File | Single responsibility | Key public types |
|---|---|---|
| `blackboard.rs` | In-memory shared channel for one run (agent↔agent). | `Blackboard`, `Message` |
| `board.rs` | Multi-process file board (`.ensemble/board.jsonl`, file-locked). | `FileBoard` |
| `ndjson.rs` | Generic append-only NDJSON feed primitive (locking discipline). | `Feed` |
| `ledger.rs` | SQLite coordination ledger (task state machine). | `Ledger`, `Task`, `TaskState`, `Counts` |
| `journal.rs` | Per-run JSONL flight-recorder log. | `Entry` |

### Mesh / networking / serving
| File | Single responsibility | Key public types |
|---|---|---|
| `serve.rs` | HTTP agent host (`/run`, `/health`, `/control`). | `BindAddr` |
| `service.rs` | Platform service unit generation (Windows/launchd/systemd). | `ServeServiceConfig` |
| `discovery.rs` | Tailscale node discovery (rate-limited status capture). | `Node` |
| `mesh.rs` | Render mesh / `up` views (local + remote CLIs). | `render_mesh()`, `render_up()` |
| `council.rs` | Council broadcast (fan one prompt to all reachable CLIs). | `CouncilTarget` |
| `wire.rs` | Request/response DTOs (`/run`, `/control`, git-sync). | `RunRequest`, `RunResponse`, `ControlPlaneRequest/Response` |
| `repo_sync.rs` | git-bundle cross-machine sync (conductor ↔ node). | `head_sha()`, `bundle_rev()`, `merge_branch()` |
| `worktree.rs` | git worktree isolation + lifecycle. | `Worktree`, `KeptWorktree` |

### Control plane / supervision / team
| File | Single responsibility | Key public types |
|---|---|---|
| `control_plane.rs` | Stable team/control-plane interface (local + remote impls). | `ControlPlane`, `LocalControlPlane`, `RemoteControlPlane` |
| `supervise.rs` | Live-supervision schema + event rendering. | `StreamEvent`, `ControlCmd`, `ControlState` |
| `supervisor.rs` | Supervisor-AI prompt + report structures. | `SupervisorEvidence`, `SupervisorReport`, `SupervisorRecommendation` |
| `team.rs` | Team session / members / status / inbox. | `TeamSession`, `TeamStatus`, `TeamInbox` |

### MCP / install / doctor
| File | Single responsibility | Key public types |
|---|---|---|
| `mcp.rs` | MCP stdio server (protocol + tool list + tool calls). | `Ctx`, `CrewRunner`, `SupervisorRunner` |
| `mcp_install.rs` | Generate/merge MCP install config per client (idempotent). | `ClientKind`, `InstallParams` |
| `doctor.rs` | Environment readiness checks (CLIs, tailscale, git). | `ToolStatus` |

---

## `tests/` — integration / e2e
| File | Covers |
|---|---|
| `pipeline_hermetic.rs` | Hermetic role-pipeline run (no real CLIs). |
| `test_gate_e2e.rs` | Test-gate firewall end-to-end. |
| `ledger_dispatch.rs` | Durable dispatch + ledger recovery. |
| `cross_machine.rs` | Cross-machine remote-adapter / git-sync. |
| `firewall_b.rs` | Firewall-B (circuit breaker) behavior. |
| `live_smoke.rs` | Live smoke against real CLIs (gated). |
| `version_cli.rs` | `--version` CLI output. |

## `docs/` — design, plans, specs, runbooks, ADRs
| Path | What |
|---|---|
| `docs/2026-06-19-ensemble-design.md` | Original system design. |
| `docs/decisions/ADR-001-control-plane-boundary.md` | ADR: control-plane boundary. |
| `docs/plans/*.md` | Phased implementation plans (phase1 → stage2 federation, dated). |
| `docs/specs/*.md` | Feature specs (MCP design, firewalls/test-gate, OSS onboarding, two-phase tests, live supervision). |
| `docs/RUNBOOK-*.md` | Operator runbooks (live MCP crew, single-machine team CLI). |
| `docs/FLEET-SSH-METHOD.md` | Fleet SSH method notes. |

> Untracked (working notes, not in git): `docs/RUNBOOK-phase1-phase2-manual-testing.md`.

## `scripts/` — PowerShell ops/verify
| File | What |
|---|---|
| `install.ps1` / `uninstall.ps1` | Install/remove the binary + PATH on Windows. |
| `doctor`-adjacent: `phase1-verify.ps1`, `phase2-verify.ps1`, `phase2-goal-shape.ps1`, `phase2-local-ready.ps1`, `phase2-run-evidence.ps1` | Phase verification gates. |
| `phase2-fleet.ps1`, `phase2-phantom-bootstrap.ps1`, `phantom-single-machine.ps1` | Fleet / phantom bootstrap. |
| `acceptance-single-machine.ps1`, `smoke.ps1` | Acceptance / smoke. |

## `examples/` — sample crew configs
| File | What |
|---|---|
| `examples/crew.toml`, `examples/crew-phase2.toml` | Example crew pipelines. |
| `examples/phase2-fleet.sample.json` | Example fleet topology. |

## `.github/` — CI
| File | What |
|---|---|
| `.github/workflows/ci.yml` | Build + test CI. |

## Root files
| File | What |
|---|---|
| `Cargo.toml` | Crate manifest. |
| `README.md` | Public README. |
| `index.md` | **This file.** |
| `AGENTS.md`, `CONTRIBUTING.md` | Contributor guidance. |
| `crew-main.toml`, `crew-sat-two-ai.toml`, `phase2-slice-b-crew.toml` | Shipped crew configs. |
| `LICENSE`, `NOTICE` | Apache-2.0 license + notice. |

---

## Known duplication — single-source-of-truth debt

These are **confirmed** spots where one responsibility is implemented in more than one place,
i.e. they violate "one function = one file/owner". Fix forward; do not add new copies.

| # | Sev | Status | What is duplicated | Files | Note |
|---|---|---|---|---|---|
| 4 | 🟡 | ✅ **done** | `sanitize` (non-`[A-Za-z0-9_.-]` → `-`) | `util::sanitize_component` ← `journal.rs`, `worktree.rs`, `repo_sync.rs` | Unified via `util::sanitize_component(s, allow_dot)`; callers keep their own truncate/fallback. |
| 5 | 🟡 | ✅ **done** | Text truncation/excerpt/tail | `util::excerpt` / `util::tail` ← `blackboard.rs`, `board.rs`, `test_gate.rs` | Head-truncate (`excerpt`) and tail-truncate (`tail`) now live once in `util.rs`. |
| 2 | 🟡 | ◑ **partly done / overstated** | IPv4-literal predicate | `util::is_ipv4_literal` ← `council.rs`, `main.rs` | NOTE: `main.rs::normalized_control_host` already *reuses* `council::short_host` — it was never a full reimplementation. Only the dotted-numeric `is_ipv4` predicate was duplicated; now extracted. Nothing further to merge here. |
| 1 | 🔴 | ☐ todo | Append-only NDJSON locking (append-terminated, valid-line count, cursor) | `ndjson.rs`, `board.rs` | `ndjson.rs` header admits it "mirrors board.rs:115-141 deliberately". Plan: move `board.rs` onto `ndjson::Feed`. **Higher risk** (multi-process locking) — verify with `board::tests` + concurrency tests. |
| 3 | 🔴 | ☐ todo | CLI / control routing logic living in the entrypoint | `main.rs` (~5k lines: `route_control_member*`, `control_token*`, arg parsers) | Extract to a `control_routing` / per-command module; belongs in lib, not `main`. Large mechanical move. |
| 6 | 🟡 | ☐ todo | git/command-run wrappers | `repo_sync.rs` (`git_capture`/`git_run`), `main.rs` (`run_command*` ×5), `discovery.rs::capture_bounded`, `test_gate.rs::shell` | One `process`/`CommandRunner` abstraction. |
| 7 | 🟡 | ☐ todo | Adapter ctor/timeout/abort boilerplate | `exec_adapter.rs`, `agy_adapter.rs`, `remote_adapter.rs` | Shared `with_timeout`/`set_abort` pattern → factory or default-impl. |

> One-function-one-object check: per-run state is currently spread across `Conductor.stream`,
> `Conductor.control`, the on-disk `.ensemble/stream|control/*.ndjson` feeds, and local
> `ControlState` / `TeamStatus` — kept in sync by hand. A single `RunState` owner would remove
> that manual syncing. (Larger refactor — tracked here, not yet done.)
