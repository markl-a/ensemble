# ADR-001: Add a Control Plane Boundary for Local and Remote Teams

## Status
Accepted

## Date
2026-06-22

## Context
Phase 1 makes multiple local AI CLIs collaborate through repo-local `.ensemble/` state:
team board, stream feeds, and control feeds. That works for one machine, but Phase 2 needs the same
operations across machines. If CLI, MCP, supervisor, and remote code each keep writing files directly,
multi-machine support would fork the behavior and make `steer` / `abort` inconsistent.

The user-facing commands should stay stable:

- `ensemble team status|say|inbox`
- `ensemble watch <name>`
- `ensemble steer <name> "<prompt>"`
- `ensemble abort <name> [--hard]`
- MCP tools for team/status/inbox/watch/steer/abort

## Decision
Introduce a `ControlPlane` interface with a `LocalControlPlane` implementation.

The interface owns these operations:

- read team status
- post team messages
- read team inbox
- read stream lines
- append control commands

`LocalControlPlane` keeps the current Phase 1 storage format under `.ensemble/`. Existing public
functions in `team.rs` remain available and delegate to the local control plane for compatibility.
CLI, MCP, and supervisor callers use the control-plane boundary instead of manually opening stream
or control feeds.

## Alternatives Considered

### Keep direct file access everywhere
- Pros: smallest immediate change
- Cons: every future remote feature would need many call-site changes and could diverge from local
  behavior
- Rejected because local and multi-machine control must share semantics.

### Replace `.ensemble/` with a central server immediately
- Pros: remote coordination becomes first-class now
- Cons: too large for the current Phase 1 stabilization; risks breaking single-machine workflows
- Rejected in favor of an incremental boundary that preserves current behavior.

## Consequences
- Phase 1 remains local-first and file-backed.
- Phase 2 can add an HTTP or coordinator-backed control plane without changing operator-facing
  commands.
- Tests can verify local behavior through the same interface future transports must implement.
- The controlled PTY receiver still reads its local control feed because it is the local transport
  endpoint that drives a child process on that machine.

## Phase 2 Slice 1
Expose the same control-plane contract through `ensemble serve`:

- `POST /control`
- request variants: `team_status`, `team_say`, `team_inbox`, `watch`, `append_control`
- response shape: `{ ok, status?, inbox?, stream?, next?, errorKind?, error? }`

The first remote transport is deliberately node-local: it reads and mutates the target node's
repo-local `.ensemble/` state. A later coordinator can implement the same contract for shared
multi-machine state.

Every Phase 2 slice must keep the Phase 1 focused regression gate green for the local single-machine
workflow: team board, MCP tools, controlled launcher parsing, watch, steer, and abort.

## Next Steps
- Add `--node` or team-member routing where an operation targets another machine.
- Add authentication and network boundary checks before accepting remote control mutations.
