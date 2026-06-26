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

## Phase 2 Slice 2
Expose the remote control plane through the operator-facing CLI:

- `ensemble team status|say|inbox --node <host|url>`
- `ensemble watch <member> --node <host|url>`
- `ensemble steer <member> "<prompt>" --node <host|url>`
- `ensemble abort <member> [--hard] --node <host|url>`

The default remains local and file-backed. When `--node` is present, the CLI normalizes a bare host
to `http://<host>:7878` and sends the same `ControlPlane` operations through `RemoteControlPlane`.
Full URLs are used as explicit base URLs. `--node auto` is rejected for these control commands until
member/discovery routing is implemented, so an operator does not accidentally target a host named
`auto`. Only `--node local` bypasses HTTP and forces the file-backed local plane; explicit loopback
targets such as `--node localhost`, `--node 127.0.0.1`, and `--node ::1` still route through HTTP so
the remote control/token boundary can be tested locally.

Internal supervision paths that already own a local child process still append to the local control
feed. Remote control is an explicit operator routing choice in this slice.

Verification for this slice includes:

- parser coverage for `team --node` and `watch --node`;
- URL normalization tests for bare hosts and explicit URLs;
- a fake `/control` HTTP test proving the CLI helper posts `append_control` through the remote
  contract;
- Phase 1 focused regression: `control`, `team_`, and `launcher`.

## Phase 2 Slice 3
Add a minimal shared-secret boundary for remote control mutations.

Remote `/control` still supports read-only operations without a token:

- `team_status`
- `team_inbox`
- `watch`

Mutation operations require a matching token when the server is configured with one:

- `team_say`
- `append_control`

The server reads the token from `ENSEMBLE_TOKEN` or from `ensemble serve|up --token <token>`.
The client sends the token through the `x-ensemble-token` HTTP header when `--token <token>` is
present on `team`, `watch`, `steer`, or `abort`, or when `ENSEMBLE_TOKEN` is set. Blank tokens and
tokens containing control characters are ignored per source, so an invalid explicit `--token` does
not suppress a valid `ENSEMBLE_TOKEN`. Tokens are not printed in error messages or logs.

This is deliberately not a full identity system. It is the smallest real boundary for cross-machine
steer/abort/team mutations while keeping the existing tailnet trust model and local Phase 1 defaults.

Verification for this slice includes:

- configured server rejects tokenless `append_control`;
- configured server accepts `append_control` with a matching token;
- configured server still allows read-only `watch` without a token;
- remote client sends `x-ensemble-token`;
- `watch --token <token>` parsing does not treat the token as a member name;
- an invalid explicit token falls back to a valid `ENSEMBLE_TOKEN`;
- token normalization rejects blanks and control characters.

## Phase 2 Slice 4
Add member-address routing for live control commands.

The live-control commands now accept a stable `member@node` address in addition to explicit
`--node <host|url>`:

- `ensemble watch <member@node>`
- `ensemble steer <member@node> "<prompt>"`
- `ensemble abort <member@node> [--hard]`

`--node` remains the highest-precedence routing decision. When no explicit node is supplied, the CLI
infers the route from the suffix after the final `@`. The member name itself is preserved when sent
to the target control plane, so a remote node that launched `claude@macbook` still reads and mutates
the `claude@macbook` stream/control feeds. The suffix is only a routing hint.

If the suffix matches a host already visible in the mesh, the CLI uses the discovered `serve` URL
instead of reconstructing `http://<node>:7878`. This keeps MagicDNS, tailnet-IP fallback, and future
discovery port behavior centralized in `discover_mesh`. If discovery has no matching host, the CLI
falls back to the suffix as a bare host so explicit operator knowledge still works.

To preserve the Phase 1 local workflow, `@local` and a suffix matching this machine's short hostname
continue to use the local file-backed plane. This avoids turning local default member names such as
`codex@conductor` into remote HTTP calls on the same machine.

Operators can also force the local plane with `--node local`. This is the escape hatch for existing
local member names that contain an `@` suffix which looks like a node name, for example
`reviewer@work`. Other explicit loopback node values are not escape hatches; they remain HTTP
targets.

This slice intentionally applies only to member-targeted live-control commands. `team
status|say|inbox` remains routed by `--node`, because those operations target team state rather than
a member identity.

Verification for this slice includes:

- `member@node` infers the remote node when `--node` is absent;
- discovered mesh URLs are preferred for the inferred node;
- explicit `--node` overrides the member suffix;
- `--node local` forces the local file-backed plane, while explicit loopback targets remain remote
  HTTP routes;
- `@local` and the local short hostname stay on the local control plane;
- Phase 1 focused regression: `control`, `team_`, and `launcher`.

## Next Steps
- Decide the shared/coherent multi-node team-state model beyond node-local `.ensemble/` state.
