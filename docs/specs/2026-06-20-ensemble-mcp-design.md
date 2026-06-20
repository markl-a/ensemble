# ensemble mcp — crew-participation API (design)

> Status: DESIGN (approved-to-build; transport decision delegated to + decided by the implementer
> 2026-06-20). Realizes Phase-1 step 3 of `2026-06-20-two-phase-real-tests-design.md` (decision 1:
> a LIVE CLI becomes a first-class crew member via MCP).

## Goal

A live interactive CLI (claude / codex / opencode as an MCP client) becomes a first-class crew
member: it reads the mesh + the shared blackboard, claims/assigns work, gets its OWN git worktree,
posts results, and merges — all via MCP tools exposed by `ensemble mcp` (a stdio MCP server each live
CLI launches). ensemble ALSO drives headless workers (the conductor) over the SAME primitives, so
live members and spawned workers share one substrate.

## Transport decision: hand-rolled JSON-RPC stdio + thread-per-request (NOT tokio/rmcp)

The operator's goal is "async but as real-time as possible" — i.e. CONCURRENT + non-blocking + timely
request handling, so a long `ensemble_run`/`ensemble_merge` a member kicks off never blocks its
concurrent `ensemble_board_read` polls. That needs concurrent in-flight request handling within one
stdio server (JSON-RPC pairs by id, so out-of-order responses are legal) — which does NOT require an
async runtime.

**Decision: a hand-rolled minimal MCP stdio server** — a synchronous reader loop over
newline-delimited JSON-RPC 2.0 on stdin; each request dispatched to its OWN `std::thread`; responses
written to stdout under a `Mutex` (one complete line per message). Rationale:
- ensemble's primitives (git, rusqlite, the conductor) are all SYNCHRONOUS + BLOCKING. Under tokio
  every one would need `spawn_blocking`; a thread is the natural concurrency primitive for blocking
  IO and runs them directly.
- Keeps ensemble's no-tokio, single-binary, minimal-dependency ethos (only `serde_json`, already a
  dep). No async refactor of a deliberately-sync codebase.
- Delivers the concurrency the operator wants: a long tool call on its own thread never stalls a
  concurrent quick one.
- rmcp's only real edge — server-INITIATED push notifications + full protocol coverage — is not
  needed for Phase-1 (members poll `board_read`); revisit if push becomes necessary.

Subset implemented: `initialize`, `notifications/initialized`, `tools/list`, `tools/call` (+ JSON-RPC
error objects for unknown method / bad params / tool error). The server echoes the client's
`protocolVersion` when supported, else a pinned default.

## Coordination substrate (the core design)

Live CLIs each spawn their OWN `ensemble mcp` subprocess → they do NOT share memory → they coordinate
through shared ON-DISK state under the repo's `.ensemble/` (already the home of worktrees + run
journals).

- **Session = the repo (cwd).** Every `ensemble mcp` launched in the same repo shares
  `.ensemble/board.jsonl` (the live blackboard) and `.ensemble/claims.db` (the ledger). Phase-1
  default: one session per repo. (A named `--session <id>` for multiple concurrent crews in one repo
  is a later refinement.)
- **FileBoard** — a persistent, multi-process append-only JSONL board reusing `blackboard::Message`.
  `post` appends one line under an advisory lockfile (`.ensemble/board.lock`); `read_since(n)` returns
  messages at index ≥ n. Concurrent-safe across processes (lock) and threads.
- **claim** — at-most-once via the existing SQLite `Ledger` (`.ensemble/claims.db`).
- **worktree** — a KEPT worktree (does NOT remove on drop; the member owns its lifecycle — cleaned up
  by a merge or a future gc). A small addition to `worktree.rs` (create-without-RAII-removal).

## The 7 tools (thin over existing primitives)

| Tool | Maps to | Status |
|---|---|---|
| `ensemble_mesh` | `discovery::discover_mesh` + `present_clis` | built |
| `ensemble_board_read {since}` | `FileBoard::read_since` | new (read) |
| `ensemble_board_post {kind, body}` | `FileBoard::post` | new (write) |
| `ensemble_claim {task_id, descr}` | `ledger::Ledger` claim | built |
| `ensemble_worktree {task}` | kept `Worktree` (path + branch) | built + small add |
| `ensemble_merge {branch, into?, resolver?}` | `merge_branch` / `merge_with_resolver` | built |
| `ensemble_run {task}` | `Conductor` headless run | built |

Each tool call carries the repo via the server's launch cwd (or an explicit `--repo`); the `from`
identity for board posts is the server's `--name <agent>` (defaults to a generated id).

## Incremental slices (each ends runnable + double-gated)

1. **Server scaffold + read-only** — the JSON-RPC stdio loop (initialize / tools.list / tools.call /
   errors), thread-per-request dispatch, `ensemble mcp` command; tools `ensemble_mesh` +
   `ensemble_board_read` (with the FileBoard READ side). Hermetic tests on the pure dispatch
   (initialize handshake, tools.list shape, unknown-method error, tools.call routing, board_read).
2. **`ensemble_board_post`** — the FileBoard WRITE side (locked append); multi-process append test.
3. **`ensemble_claim` + `ensemble_worktree`** — ledger claim tool + kept-worktree tool.
4. **`ensemble_merge` + `ensemble_run`** — land/merge + spawn a headless crew run.

## Testability

The protocol is split so the I/O shell is thin: `dispatch(method, params, &Ctx) -> Result<Value,
RpcError>` is pure-given-Ctx and hermetically tested (handshake, list, routing, errors, FileBoard
read/post on a temp repo). The stdio reader loop + thread spawn + stdout `Mutex` is the thin shell.
`ensemble_mesh` (shells tailscale) is covered structurally; FileBoard + claim + worktree + merge are
file/db-based and hermetic.

## Deferred

Server-initiated push notifications (rmcp territory); a named multi-session id; cross-machine board
(Phase-2 shared ledger); auth (Phase-2 serve-auth). 
