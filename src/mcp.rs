//! `ensemble mcp` — a minimal, hand-rolled MCP (Model Context Protocol) server over stdio that makes
//! a LIVE CLI a first-class crew member (design `2026-06-20-ensemble-mcp-design.md`). Newline-delimited
//! JSON-RPC 2.0 on stdin/stdout; each request is dispatched on its OWN thread (responses serialized by
//! a stdout `Mutex`) so a long tool call never blocks a concurrent quick one — the operator's
//! "async but as real-time as possible" goal, without dragging in an async runtime (ensemble's
//! primitives are synchronous + blocking, so a thread is the natural concurrency unit).
//!
//! Slice 1 implements the protocol subset (`initialize`, `notifications/initialized`, `tools/list`,
//! `tools/call`) + the read-only tools `ensemble_mesh` and `ensemble_board_read`.

use crate::board::FileBoard;
use fs2::FileExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// The MCP protocol version we advertise when a client doesn't request one.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

/// Max in-flight request threads. A client could otherwise pipeline unboundedly and exhaust threads;
/// the reader loop blocks (backpressure) once this many are running.
const MAX_INFLIGHT: usize = 16;
const DEFAULT_MCP_LIMIT: usize = 50;
const MAX_MCP_LIMIT: usize = 200;

/// A tiny counting semaphore (std only) to cap concurrent request handlers.
struct Semaphore {
    permits: Mutex<usize>,
    cv: Condvar,
}
impl Semaphore {
    fn new(n: usize) -> Self {
        Self {
            permits: Mutex::new(n),
            cv: Condvar::new(),
        }
    }
    /// Block until a permit is free, take it, and return an RAII guard that releases it on Drop —
    /// so the permit is returned whether the handler completes normally, PANICS (Drop runs during
    /// unwind), or is dropped unspawned. Without the guard a panicking tool call would leak its
    /// permit and, after MAX_INFLIGHT panics, wedge the reader forever on `acquire`.
    fn acquire(self: &Arc<Self>) -> PermitGuard {
        let mut p = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        while *p == 0 {
            p = self.cv.wait(p).unwrap_or_else(|e| e.into_inner());
        }
        *p -= 1;
        PermitGuard(self.clone())
    }
    fn release(&self) {
        let mut p = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        *p += 1;
        self.cv.notify_one();
    }
}

/// Returns a `Semaphore` permit on Drop (panic-safe).
struct PermitGuard(Arc<Semaphore>);
impl Drop for PermitGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Per-server config: the repo (= crew session), this member's identity for board posts/claims, and
/// the delegate `ensemble_run` uses to spawn a governed crew sub-run.
pub struct Ctx {
    pub repo: PathBuf,
    pub name: String,
    pub team: String,
    /// How `ensemble_run` delegates a governed crew sub-run. The binary always wires this (a
    /// `Conductor` adapter); it is `None` only in hermetic unit tests of the OTHER tools, where an
    /// `ensemble_run` call returns an internal error rather than running anything.
    pub runner: Option<Arc<dyn CrewRunner>>,
    /// Optional supervisor runner. When absent, `ensemble_supervise` is not advertised because this
    /// library module deliberately does not know how to construct vendor adapters.
    pub supervisor: Option<Arc<dyn SupervisorRunner>>,
}

/// The capability the MCP server uses to delegate a whole governed crew run for `ensemble_run`. The
/// real implementation (in the `ensemble` binary's `mcp` command) wraps a `Conductor` built from the
/// repo's crew.toml + the local adapter registry; unit tests inject a fake. It is a trait so this
/// module stays free of crew/adapter construction (which lives in the binary) and so a real,
/// minutes-long, multi-CLI crew run is never invoked from a hermetic unit test.
pub trait CrewRunner: Send + Sync {
    /// Run ONE governed task to a terminal decision in an isolated throwaway worktree of `repo`
    /// (delegates to `Conductor::run_in_repo`). Returns the flat summary `ensemble_run` serializes.
    fn run(&self, task: &str, repo: &Path) -> RunSummary;
}

/// The flat outcome of an `ensemble_run` delegation — the slice of `conductor::RunOutcome` a member
/// needs to decide what to do next (merge the kept branch, or act on the escalation reason).
#[derive(Debug, Clone)]
pub struct RunSummary {
    /// Whether the gate LANDED the work (else it escalated or ran out of rounds).
    pub landed: bool,
    /// How many implementer→review rounds the run took.
    pub rounds: u32,
    /// On LAND, the `ensemble/<slug>` branch the committed work was kept on (land it with
    /// `ensemble_merge`); `None` on escalation.
    pub branch: Option<String>,
    /// On escalation, the human-readable reason; empty when landed.
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct SuperviseRequest {
    pub name: String,
    pub team: Option<String>,
    pub agent: String,
    pub since: usize,
    pub apply_steer: bool,
    pub abort_on_critical: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperviseSummary {
    pub name: String,
    pub team: String,
    pub agent: String,
    pub recommendation: crate::SupervisorRecommendation,
    pub reason: String,
    pub steer: Option<String>,
    pub critical: bool,
    pub board_next: usize,
    pub control_next: Option<usize>,
}

pub trait SupervisorRunner: Send + Sync {
    fn supervise(
        &self,
        req: SuperviseRequest,
        repo: &Path,
        caller: &str,
    ) -> Result<SuperviseSummary, String>;
}

/// A JSON-RPC error object (code + message).
pub struct RpcError {
    pub code: i64,
    pub message: String,
}
impl RpcError {
    fn method_not_found(m: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {m}"),
        }
    }
    fn invalid_params(m: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: m.into(),
        }
    }
    fn internal(m: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: m.into(),
        }
    }
}

/// Route a JSON-RPC method to its result. Pure given `ctx` (no stdio) — the unit of the test suite.
pub fn dispatch(method: &str, params: &Value, ctx: &Ctx) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(initialize_result(params)),
        "tools/list" => Ok(tools_list(ctx)),
        "tools/call" => tools_call(params, ctx),
        other => Err(RpcError::method_not_found(other)),
    }
}

fn initialize_result(params: &Value) -> Value {
    // Echo the client's requested protocol version when present (MCP convention), else our default.
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL)
        .to_string();
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ensemble", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn tools_list(ctx: &Ctx) -> Value {
    let mut out = json!({ "tools": [
        {
            "name": "ensemble_mesh",
            "description": "List this node's local AI CLIs and which agents each tailnet peer hosts.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "ensemble_board_read",
            "description": "Read the shared crew blackboard for this repo. Returns messages at index >= `since`, each with its absolute index, plus the `next` cursor to poll from.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since": { "type": "integer", "minimum": 0, "description": "return messages from this index onward (default 0)" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_board_post",
            "description": "Post a message to the shared crew blackboard for this repo, attributed to THIS member. `kind` is a short tag (e.g. result | verdict | question | plan | finding); `body` is the message (excerpted if very long). Returns the new board length as `next` — the cursor to poll `ensemble_board_read` from.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "short tag, e.g. result | verdict | question | plan | finding" },
                    "body": { "type": "string", "description": "the message text" }
                },
                "required": ["kind", "body"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_team_status",
            "description": "Read repo-local team state for this MCP server's team: board length, ledger counts, and known stream/control feeds. Optional `team` overrides the server default for one call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "description": "team name to inspect; defaults to this server's team" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_team_say",
            "description": "Post a message to the repo-local team board as THIS MCP member. `body` is required; `kind` defaults to note. The author is always the server identity, never a client-supplied field.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "description": "team name; defaults to this server's team" },
                    "kind": { "type": "string", "description": "short tag for the message (default note)" },
                    "body": { "type": "string", "description": "message body" }
                },
                "required": ["body"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_team_inbox",
            "description": "Read bounded messages from the repo-local team board with a cursor. Returns at most `limit` messages and a `next` cursor for polling.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "team": { "type": "string", "description": "team name; defaults to this server's team" },
                    "since": { "type": "integer", "minimum": 0, "description": "return messages from this cursor (default 0)" },
                    "limit": { "type": "integer", "minimum": 0, "maximum": 200, "description": "maximum messages to return (default 50)" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_watch",
            "description": "Read bounded raw stream events for a live run/member name from the same stream feed used by `ensemble watch`. Returns JSON events with indexes and a `next` cursor.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "watch/run/member name" },
                    "since": { "type": "integer", "minimum": 0, "description": "return events from this cursor (default 0)" },
                    "limit": { "type": "integer", "minimum": 0, "maximum": 200, "description": "maximum events to return (default 50)" }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_steer",
            "description": "Append a steer command to the same control feed used by `ensemble steer`. The command is attributed to THIS MCP member.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "watch/run/member name to steer" },
                    "prompt": { "type": "string", "description": "instruction to inject into the next round" }
                },
                "required": ["name", "prompt"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_abort",
            "description": "Append an abort command to the same control feed used by `ensemble abort`. Clean abort waits for a round boundary; `hard` asks the running adapter to stop immediately.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "watch/run/member name to abort" },
                    "hard": { "type": "boolean", "description": "kill the running CLI immediately when true (default false)" }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_worktree",
            "description": "Create (or idempotently re-attach to) THIS member's persistent git worktree for an isolated task branch in this repo. Returns {path, branch, slug}. The worktree persists on disk across calls and `ensemble mcp` restarts; edit + commit there, then land it with `ensemble merge`. Idempotent per (member, task).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "short label for the workspace/branch (default \"work\")" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_enqueue",
            "description": "Add a task to the repo's shared crew work-queue (a durable SQLite ledger). Idempotent: the task id is a stable hash of `descr`, so enqueuing the same text twice is a no-op. Returns {enqueued, id}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "descr": { "type": "string", "description": "the task description" }
                },
                "required": ["descr"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_claim",
            "description": "Atomically claim the oldest unclaimed task from the repo's shared crew work-queue, as THIS member, AT-MOST-ONCE (no two members ever get the same task). Returns {claimed:true, id, descr} or {claimed:false} when the queue is empty.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "ensemble_merge",
            "description": "Land a member's branch (e.g. a kept worktree's `ensemble/<member>/<task>`) onto a target branch (default \"main\") in this repo — fast-forward or true-merge. On CONFLICT the merge is ABORTED and the worktree restored (NEVER auto-resolved); the conflicting paths are returned so you can escalate or resolve, then retry. Concurrent merges are serialized. Returns {landed:true, branch, into} or {landed:false, branch, into, conflict:[paths]}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "branch": { "type": "string", "description": "the branch to land (e.g. ensemble/<member>/<task>)" },
                    "into": { "type": "string", "description": "target branch to land onto (default \"main\")" }
                },
                "required": ["branch"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_complete",
            "description": "Record a TERMINAL success for a task THIS member claimed: mark it done in the shared crew work-queue with `outcome` (e.g. the landed branch). Ownership-guarded — only the member that claimed the task can complete it, and only while it is still claimed (a no-op otherwise, so it can't overwrite another member's task or re-finish a done one). Returns {completed:true, id} or {completed:false, id, detail}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the task id (from ensemble_claim/ensemble_enqueue)" },
                    "outcome": { "type": "string", "description": "the terminal result, e.g. \"LANDED ensemble/<member>/<task>\"" }
                },
                "required": ["id", "outcome"],
                "additionalProperties": false
            }
        },
        {
            "name": "ensemble_fail",
            "description": "Record a TERMINAL failure for a task THIS member claimed: mark it failed in the shared crew work-queue with `reason`. Ownership-guarded exactly like ensemble_complete (only the claiming member, only while still claimed). Returns {failed:true, id} or {failed:false, id, detail}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the task id (from ensemble_claim/ensemble_enqueue)" },
                    "reason": { "type": "string", "description": "why it failed, e.g. \"ESCALATED: tests never passed\"" }
                },
                "required": ["id", "reason"],
                "additionalProperties": false
            }
        }
    ]});
    // `ensemble_run` is advertised ONLY when a crew runner is configured (see `mcp_runner`: a missing
    // crew.toml leaves it `None` while the server still serves the other tools). tools/list is a
    // capability contract — never promise a tool that a call would reject with -32603 "not configured".
    if ctx.runner.is_some() {
        if let Some(tools) = out["tools"].as_array_mut() {
            tools.push(json!({
                "name": "ensemble_run",
                "description": "Delegate a task to a HEADLESS governed crew sub-run in this repo: the full implementer → test-gate → reviewers → gate pipeline, in its own throwaway git worktree. BLOCKS until the sub-run reaches a terminal decision (it runs on its own thread, so your concurrent board polls still flow). On LAND the committed work is kept on a branch you can then land with ensemble_merge. Returns {landed:true, rounds, branch} or {landed:false, rounds, reason}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "the task to delegate to the crew" }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }
            }));
        }
    }
    if ctx.supervisor.is_some() {
        if let Some(tools) = out["tools"].as_array_mut() {
            tools.push(json!({
                "name": "ensemble_supervise",
                "description": "Ask a configured supervisor agent to inspect recent stream events, team-board messages, git status, and diff summary for a live run/member. Advisory by default; set applySteer or abortOnCritical to allow control-feed mutation from parsed recommendations.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "watch/run/member name to inspect" },
                        "team": { "type": "string", "description": "team name; defaults to this server's team" },
                        "agent": { "type": "string", "description": "local supervisor agent to ask (default claude)" },
                        "since": { "type": "integer", "minimum": 0, "description": "stream cursor to inspect from (default 0)" },
                        "applySteer": { "type": "boolean", "description": "write a steer command only when the parsed recommendation is steer (default false)" },
                        "abortOnCritical": { "type": "boolean", "description": "write a hard abort only for an explicit critical abort recommendation (default false)" }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            }));
        }
    }
    out
}

fn tools_call(params: &Value, ctx: &Ctx) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| RpcError::invalid_params("tools/call: missing tool name"))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let text = match name {
        "ensemble_mesh" => tool_mesh(),
        "ensemble_board_read" => tool_board_read(&args, ctx)?,
        "ensemble_board_post" => tool_board_post(&args, ctx)?,
        "ensemble_team_status" => tool_team_status(&args, ctx)?,
        "ensemble_team_say" => tool_team_say(&args, ctx)?,
        "ensemble_team_inbox" => tool_team_inbox(&args, ctx)?,
        "ensemble_watch" => tool_watch(&args, ctx)?,
        "ensemble_steer" => tool_steer(&args, ctx)?,
        "ensemble_abort" => tool_abort(&args, ctx)?,
        "ensemble_supervise" => tool_supervise(&args, ctx)?,
        "ensemble_worktree" => tool_worktree(&args, ctx)?,
        "ensemble_enqueue" => tool_enqueue(&args, ctx)?,
        "ensemble_claim" => tool_claim(&args, ctx)?,
        "ensemble_merge" => tool_merge(&args, ctx)?,
        "ensemble_complete" => tool_complete(&args, ctx)?,
        "ensemble_fail" => tool_fail(&args, ctx)?,
        "ensemble_run" => tool_run(&args, ctx)?,
        other => return Err(RpcError::invalid_params(format!("unknown tool: {other}"))),
    };
    // MCP tool result: a content list. We return one text item (JSON-or-text payload).
    Ok(json!({ "content": [ { "type": "text", "text": text } ], "isError": false }))
}

fn tool_mesh() -> String {
    let local = crate::doctor::present_clis();
    let hosts = crate::discovery::discover_mesh(7878);
    crate::mesh::render_mesh(&local, &hosts)
}

fn tool_board_read(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    // arguments must be an object (or absent/null); a bad shape is a client error, not a silent reset.
    if !(args.is_null() || args.is_object()) {
        return Err(RpcError::invalid_params("arguments must be an object"));
    }
    let since = match args.get("since") {
        None | Some(Value::Null) => 0usize,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| RpcError::invalid_params("`since` must be a non-negative integer"))?
            as usize,
    };
    let board = FileBoard::open(&ctx.repo);
    let all = board
        .read_since(0)
        .map_err(|e| RpcError::internal(format!("board read: {e}")))?;
    let next = all.len();
    let messages: Vec<Value> = all
        .iter()
        .enumerate()
        .skip(since)
        .map(|(i, m)| json!({ "index": i, "from": m.from, "kind": m.kind, "body": m.body }))
        .collect();
    Ok(json!({ "messages": messages, "next": next }).to_string())
}

/// Post one message to the repo's shared crew blackboard as THIS member (`ctx.name` — the author is
/// the server's identity, never a client-supplied field, so a member can't impersonate another).
/// `kind` and `body` are required non-blank strings; any missing/null/non-string/blank field is a
/// client error (-32602), checked BEFORE the post so a malformed call never writes a junk line.
/// Returns `{posted, next}` where `next` is the cursor positioned immediately AFTER this member's
/// message, computed atomically UNDER the board's append lock (see `FileBoard::post`) — so polling
/// `ensemble_board_read` from `next` returns every later message in order, with no skips and without
/// re-returning this post, even when other members post concurrently.
fn tool_board_post(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !args.is_object() {
        return Err(RpcError::invalid_params(
            "arguments must be an object with `kind` and `body`",
        ));
    }
    let kind = required_str(args, "kind")?;
    let body = required_str(args, "body")?;
    let next = FileBoard::open(&ctx.repo)
        .post(&ctx.name, kind, body)
        .map_err(|e| RpcError::internal(format!("board post: {e}")))?;
    Ok(json!({ "posted": true, "next": next }).to_string())
}

/// Pull a REQUIRED, non-blank string field from a tools/call `arguments` object, mapping each failure
/// mode (absent, null, non-string, blank) to a precise -32602 message that names the field — so a
/// client sees what was wrong, not a generic "unknown tool".
fn required_str<'a>(args: &'a Value, field: &str) -> Result<&'a str, RpcError> {
    match args.get(field) {
        None | Some(Value::Null) => Err(RpcError::invalid_params(format!("`{field}` is required"))),
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| RpcError::invalid_params(format!("`{field}` must be a string")))?;
            if s.trim().is_empty() {
                Err(RpcError::invalid_params(format!(
                    "`{field}` must not be empty"
                )))
            } else {
                Ok(s)
            }
        }
    }
}

fn optional_str<'a>(args: &'a Value, field: &str) -> Result<Option<&'a str>, RpcError> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| RpcError::invalid_params(format!("`{field}` must be a string")))?;
            if s.trim().is_empty() {
                Err(RpcError::invalid_params(format!(
                    "`{field}` must not be empty"
                )))
            } else {
                Ok(Some(s))
            }
        }
    }
}

fn optional_usize(args: &Value, field: &str, default: usize) -> Result<usize, RpcError> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                RpcError::invalid_params(format!("`{field}` must be a non-negative integer"))
            })?;
            usize::try_from(n)
                .map_err(|_| RpcError::invalid_params(format!("`{field}` is too large")))
        }
    }
}

fn optional_bool(args: &Value, field: &str, default: bool) -> Result<bool, RpcError> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| RpcError::invalid_params(format!("`{field}` must be a boolean"))),
    }
}

fn optional_args(args: &Value) -> Result<(), RpcError> {
    if args.is_null() || args.is_object() {
        Ok(())
    } else {
        Err(RpcError::invalid_params("arguments must be an object"))
    }
}

fn required_args(args: &Value, fields: &str) -> Result<(), RpcError> {
    if args.is_object() {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "arguments must be an object with {fields}"
        )))
    }
}

fn reject_unknown_fields(args: &Value, allowed: &[&str]) -> Result<(), RpcError> {
    let Some(obj) = args.as_object() else {
        return Ok(());
    };
    for key in obj.keys() {
        if !allowed.iter().any(|field| field == key) {
            return Err(RpcError::invalid_params(format!(
                "unknown argument `{key}`"
            )));
        }
    }
    Ok(())
}

fn mcp_limit(args: &Value) -> Result<usize, RpcError> {
    let limit = optional_usize(args, "limit", DEFAULT_MCP_LIMIT)?;
    if limit > MAX_MCP_LIMIT {
        return Err(RpcError::invalid_params(format!(
            "`limit` must be <= {MAX_MCP_LIMIT}"
        )));
    }
    Ok(limit)
}

fn mcp_team(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    Ok(crate::team::default_team_name(
        optional_str(args, "team")?.or(Some(ctx.team.as_str())),
    ))
}

fn mcp_session(
    args: &Value,
    ctx: &Ctx,
    member: &str,
) -> Result<crate::team::TeamSession, RpcError> {
    let team = mcp_team(args, ctx)?;
    Ok(crate::team::resolve_team_session(
        &ctx.repo,
        Some(&team),
        "mcp",
        Some(member),
        None,
    ))
}

fn tool_team_status(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    optional_args(args)?;
    reject_unknown_fields(args, &["team"])?;
    let session = mcp_session(args, ctx, &ctx.name)?;
    let status = crate::team::team_status(&session)
        .map_err(|e| RpcError::internal(format!("team status: {e}")))?;
    serde_json::to_string(&status).map_err(|e| RpcError::internal(format!("team status json: {e}")))
}

fn tool_team_say(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    required_args(args, "`body`")?;
    reject_unknown_fields(args, &["team", "kind", "body"])?;
    let body = required_str(args, "body")?;
    let kind = optional_str(args, "kind")?.unwrap_or("note");
    let session = mcp_session(args, ctx, &ctx.name)?;
    let next = crate::team::post_team_message(&session, &ctx.name, kind, body)
        .map_err(|e| RpcError::internal(format!("team say: {e}")))?;
    Ok(json!({ "posted": true, "team": session.team, "next": next }).to_string())
}

fn tool_team_inbox(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    optional_args(args)?;
    reject_unknown_fields(args, &["team", "since", "limit"])?;
    let since = optional_usize(args, "since", 0)?;
    let limit = mcp_limit(args)?;
    let session = mcp_session(args, ctx, &ctx.name)?;
    let mut inbox = crate::team::read_team_inbox(&session, since)
        .map_err(|e| RpcError::internal(format!("team inbox: {e}")))?;
    if inbox.messages.len() > limit {
        inbox.messages.truncate(limit);
    }
    inbox.next = since + inbox.messages.len();
    Ok(json!({ "team": session.team, "messages": inbox.messages, "next": inbox.next }).to_string())
}

fn feed_target(args: &Value) -> Result<&str, RpcError> {
    let name = required_str(args, "name")?.trim();
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return Err(RpcError::invalid_params(
            "`name` must not contain path separators or control characters",
        ));
    }
    if name.chars().all(|c| c == '.') {
        return Err(RpcError::invalid_params("`name` must not be dot-only"));
    }
    Ok(name)
}

fn tool_watch(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    required_args(args, "`name`")?;
    reject_unknown_fields(args, &["name", "since", "limit"])?;
    let name = feed_target(args)?;
    let since = optional_usize(args, "since", 0)?;
    let limit = mcp_limit(args)?;
    let feed = crate::Feed::open(crate::member_stream_path(&ctx.repo, name));
    let lines = feed
        .read_since(since)
        .map_err(|e| RpcError::internal(format!("watch: {e}")))?;
    let messages: Vec<Value> = lines
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(offset, line)| {
            let event = serde_json::from_str::<Value>(&line).unwrap_or(Value::String(line));
            json!({ "index": since + offset, "event": event })
        })
        .collect();
    let next = since + messages.len();
    Ok(json!({
        "name": name,
        "stem": crate::member_file_stem(name),
        "messages": messages,
        "next": next
    })
    .to_string())
}

fn append_control(ctx: &Ctx, name: &str, cmd: &crate::ControlCmd) -> Result<usize, RpcError> {
    let feed = crate::Feed::open(crate::member_control_path(&ctx.repo, name));
    let line = serde_json::to_string(cmd)
        .map_err(|e| RpcError::internal(format!("control encode: {e}")))?;
    feed.append(&line)
        .map_err(|e| RpcError::internal(format!("control append: {e}")))
}

fn tool_steer(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    required_args(args, "`name` and `prompt`")?;
    reject_unknown_fields(args, &["name", "prompt"])?;
    let name = feed_target(args)?;
    let prompt = required_str(args, "prompt")?;
    let cmd = crate::ControlCmd::Steer {
        from: ctx.name.clone(),
        prompt: prompt.to_string(),
    };
    let next = append_control(ctx, name, &cmd)?;
    Ok(json!({
        "steered": true,
        "name": name,
        "stem": crate::member_file_stem(name),
        "next": next
    })
    .to_string())
}

fn tool_abort(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    required_args(args, "`name`")?;
    reject_unknown_fields(args, &["name", "hard"])?;
    let name = feed_target(args)?;
    let hard = optional_bool(args, "hard", false)?;
    let cmd = crate::ControlCmd::Abort {
        from: ctx.name.clone(),
        hard,
    };
    let next = append_control(ctx, name, &cmd)?;
    Ok(json!({
        "aborted": true,
        "name": name,
        "stem": crate::member_file_stem(name),
        "hard": hard,
        "next": next
    })
    .to_string())
}

fn tool_supervise(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    required_args(args, "`name`")?;
    reject_unknown_fields(
        args,
        &[
            "name",
            "team",
            "agent",
            "since",
            "applySteer",
            "abortOnCritical",
        ],
    )?;
    let name = feed_target(args)?.to_string();
    let team = optional_str(args, "team")?
        .map(str::to_string)
        .or_else(|| Some(ctx.team.clone()));
    let agent = optional_str(args, "agent")?.unwrap_or("claude").to_string();
    let since = optional_usize(args, "since", 0)?;
    let apply_steer = optional_bool(args, "applySteer", false)?;
    let abort_on_critical = optional_bool(args, "abortOnCritical", false)?;
    let runner = ctx
        .supervisor
        .as_ref()
        .ok_or_else(|| RpcError::internal("supervisor runner is not configured"))?;
    let summary = runner
        .supervise(
            SuperviseRequest {
                name,
                team,
                agent,
                since,
                apply_steer,
                abort_on_critical,
            },
            &ctx.repo,
            &ctx.name,
        )
        .map_err(|e| RpcError::internal(format!("supervise: {e}")))?;
    serde_json::to_string(&summary).map_err(|e| RpcError::internal(format!("supervise json: {e}")))
}

/// Create (or idempotently re-attach to) THIS member's persistent worktree for an OPTIONAL `task`
/// label (default `"work"`), keyed by `(ctx.name, task)` so it survives across calls and `ensemble
/// mcp` restarts (see `worktree::ensure_kept_worktree`). An ABSENT or `null` `task` defaults to
/// `"work"` (matching `ensemble_board_read`'s optional `since`); a present non-string OR blank `task`
/// is a client error (-32602); a git/worktree failure is internal (-32603). Returns `{path, branch, slug}`.
fn tool_worktree(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !(args.is_null() || args.is_object()) {
        return Err(RpcError::invalid_params("arguments must be an object"));
    }
    let task = match args.get("task") {
        None | Some(Value::Null) => "work",
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| RpcError::invalid_params("`task` must be a string"))?;
            if s.trim().is_empty() {
                return Err(RpcError::invalid_params("`task` must not be empty"));
            }
            s
        }
    };
    let wt = crate::worktree::ensure_kept_worktree(&ctx.repo, &ctx.name, task)
        .map_err(|e| RpcError::internal(format!("worktree: {e}")))?;
    Ok(
        json!({ "path": wt.path.to_string_lossy(), "branch": wt.branch, "slug": wt.slug })
            .to_string(),
    )
}

/// Seconds since the Unix epoch — the ledger's timestamps (claim/complete times). A bad clock yields
/// 0, which only affects `recover_orphans` staleness, never claim correctness.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open the repo's shared crew ledger at `<repo>/.ensemble/ledger.db`, creating `.ensemble/` first
/// (SQLite opens/creates the DB file but NOT its parent dir). All members running `ensemble mcp` for
/// this repo share this one ledger — its SQLite IMMEDIATE transactions give at-most-once claim across
/// every connection/process, so no extra lock is needed here.
fn open_ledger(repo: &Path) -> Result<crate::ledger::Ledger, RpcError> {
    let dir = repo.join(".ensemble");
    std::fs::create_dir_all(&dir).map_err(|e| RpcError::internal(format!("ledger dir: {e}")))?;
    crate::ledger::Ledger::open(&dir.join("ledger.db"))
        .map_err(|e| RpcError::internal(format!("ledger open: {e}")))
}

/// Add a task to the repo's shared crew work-queue. `descr` is a required non-blank string; the task
/// id is `dispatch::task_id(descr)` (a stable hash), so enqueuing the same text twice is idempotent
/// (no-op). Returns `{enqueued, id}` — `enqueued` is false when that id was already present.
fn tool_enqueue(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !args.is_object() {
        return Err(RpcError::invalid_params(
            "arguments must be an object with `descr`",
        ));
    }
    let descr = required_str(args, "descr")?;
    let id = crate::dispatch::task_id(descr);
    let ledger = open_ledger(&ctx.repo)?;
    let enqueued = ledger
        .enqueue(&id, descr, now_secs())
        .map_err(|e| RpcError::internal(format!("ledger enqueue: {e}")))?;
    Ok(json!({ "enqueued": enqueued, "id": id }).to_string())
}

/// Atomically claim the oldest queued task from the repo's shared work-queue, as THIS member
/// (`ctx.name`, server-set — never a client field, so a member can't claim as someone else). The
/// ledger's IMMEDIATE transaction guarantees AT-MOST-ONCE: no two members get the same task. Returns
/// `{claimed:true, id, descr}`, or `{claimed:false}` when the queue is empty (a normal result, not an
/// error). Takes no arguments (an empty/null/object args is accepted; any other shape is -32602).
fn tool_claim(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !(args.is_null() || args.is_object()) {
        return Err(RpcError::invalid_params("arguments must be an object"));
    }
    let mut ledger = open_ledger(&ctx.repo)?;
    let claimed = ledger
        .claim(&ctx.name, now_secs())
        .map_err(|e| RpcError::internal(format!("ledger claim: {e}")))?;
    Ok(match claimed {
        Some(t) => json!({ "claimed": true, "id": t.id, "descr": t.descr }),
        None => json!({ "claimed": false }),
    }
    .to_string())
}

/// Take an EXCLUSIVE per-repo lock file `name` under the repo's COMMON git dir (the same anchor as
/// `ensure_kept_worktree`, so it serializes across threads AND processes / linked worktree roots). The
/// returned `File` holds the lock until it is dropped — released on the handler's normal return OR a
/// panic (fs2 is an OS advisory lock, freed on close).
fn lock_repo(repo: &Path, name: &str) -> Result<std::fs::File, RpcError> {
    let dir = crate::worktree::git_common_dir(repo)
        .map_err(|e| RpcError::internal(format!("git common dir: {e}")))?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(dir.join(name))
        .map_err(|e| RpcError::internal(format!("lock open: {e}")))?;
    f.lock_exclusive()
        .map_err(|e| RpcError::internal(format!("lock: {e}")))?;
    Ok(f)
}

/// Land `branch` onto `into` (default "main") in this repo via the gated `repo_sync::merge_branch`
/// (fast-forward or true-merge; on conflict it ABORTS + restores the worktree, NEVER auto-resolves).
/// The merge mutates the MAIN worktree (checkout + merge), so concurrent merges (the MCP server runs
/// requests on parallel threads) are serialized by a per-repo lock. `branch` is required; a present
/// non-string/blank `into` is -32602; a git/preflight failure (e.g. a dirty worktree, mid-merge) is
/// -32603. Returns `{landed:true, branch, into}` or, on conflict, `{landed:false, branch, into,
/// conflict:[paths]}` — a conflict is a reported OUTCOME, not an error (escalate/resolve, then retry).
fn tool_merge(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !args.is_object() {
        return Err(RpcError::invalid_params(
            "arguments must be an object with `branch`",
        ));
    }
    let branch = required_str(args, "branch")?;
    let into = match args.get("into") {
        None | Some(Value::Null) => "main",
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| RpcError::invalid_params("`into` must be a string"))?;
            if s.trim().is_empty() {
                return Err(RpcError::invalid_params("`into` must not be empty"));
            }
            s
        }
    };
    // `branch`/`into` must each be a REAL local branch. A path-like or rev value (a filename, `HEAD`,
    // a SHA, a tag, `a:b`) is dangerous: `merge_branch` runs `git checkout <into>`, and e.g.
    // `into:"f"` where `f` is a tracked FILE makes git do a PATH checkout instead of switching
    // branches — the merge then lands onto whatever branch the worktree was on while we report
    // `into:"f"` (a silent wrong-ref land). Verifying `refs/heads/<name>` exists prevents that, and
    // ALSO blocks flag-injection (the name is embedded inside a `refs/heads/...` arg, so it can never
    // be read as a leading-`-` git option). This is best-effort (a branch could vanish before the
    // locked merge, which then fails -32603) but never silently lands on the wrong ref.
    for (field, name) in [("branch", branch), ("into", into)] {
        // Reject a leading '-' FIRST: even a ref literally named e.g. `--detach` (creatable via git
        // plumbing) would, if passed raw to `git checkout`/`git merge`, be parsed as a FLAG — the
        // existence check alone doesn't stop that. A legitimate branch never starts with '-'.
        if name.starts_with('-') {
            return Err(RpcError::invalid_params(format!(
                "`{field}` must not start with '-'"
            )));
        }
        // Prove the SHORT name resolves — under git's OWN revision rules, the same ones `git
        // checkout`/`git merge` use — to EXACTLY the local branch `refs/heads/<name>`. A mere
        // `show-ref --verify refs/heads/<name>` only proves the ref EXISTS, not that the raw arg maps
        // to it: `git merge HEAD` means the special HEAD (not a branch named HEAD), a same-named tag
        // shadows the branch, a `refs/heads/main`-looking name resolves the real main, and a SHA / a
        // tracked file resolve to a commit / a path. `rev-parse --symbolic-full-name <name>` returns
        // the full ref the name actually resolves to; requiring it to equal `refs/heads/<name>` rejects
        // all of those (and a path-y `into:"f"` that would otherwise cause a `git checkout` path swap).
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&ctx.repo)
            .args([
                "rev-parse",
                "--quiet",
                "--verify",
                "--symbolic-full-name",
                name,
            ])
            .output()
            .map_err(|e| RpcError::internal(format!("git rev-parse: {e}")))?;
        if !out.status.success()
            || String::from_utf8_lossy(&out.stdout).trim() != format!("refs/heads/{name}")
        {
            return Err(RpcError::invalid_params(format!(
                "`{field}` is not a local branch: {name}"
            )));
        }
    }
    let _lock = lock_repo(&ctx.repo, "ensemble-merge.lock")?; // serialize concurrent merges
    let outcome = crate::repo_sync::merge_branch(&ctx.repo, branch, into)
        .map_err(|e| RpcError::internal(format!("merge: {e}")))?;
    Ok(match outcome {
        crate::repo_sync::MergeOutcome::Landed => {
            json!({ "landed": true, "branch": branch, "into": into })
        }
        crate::repo_sync::MergeOutcome::Conflict(paths) => {
            json!({ "landed": false, "branch": branch, "into": into, "conflict": paths })
        }
    }
    .to_string())
}

/// The advisory `detail` returned when an ownership-guarded terminal write (complete/fail) does NOT
/// take effect. The guard (`ledger::complete_owned`/`fail_owned`) is a single atomic `UPDATE ...
/// WHERE id=? AND state='claimed' AND claimed_by=?`, so a false return covers exactly these cases —
/// reported as a normal OUTCOME (like a merge conflict), not a protocol error.
const NOT_OWNED_DETAIL: &str = "task is not claimed by this member (unknown id, still queued, claimed by another, or already terminal); nothing written";

/// Mark a task THIS member claimed as DONE (terminal success) in the shared work-queue. `id` and
/// `outcome` are required non-blank strings. Ownership-guarded via `ledger::complete_owned(id,
/// ctx.name, ...)`: the write happens ONLY if the task is currently claimed by this member (the same
/// anti-impersonation guarantee as claim/board_post — the worker is `ctx.name`, never a client field),
/// so a member can't close out another's task or re-finish a terminal one. The guard is atomic in SQL.
/// Returns `{completed:true, id}` on success or `{completed:false, id, detail}` when the guard blocks
/// it (a reported outcome, not an error — the member can re-check the board/queue).
fn tool_complete(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !args.is_object() {
        return Err(RpcError::invalid_params(
            "arguments must be an object with `id` and `outcome`",
        ));
    }
    let id = required_str(args, "id")?;
    let outcome = required_str(args, "outcome")?;
    let done = open_ledger(&ctx.repo)?
        .complete_owned(id, &ctx.name, outcome, now_secs())
        .map_err(|e| RpcError::internal(format!("ledger complete: {e}")))?;
    Ok(if done {
        json!({ "completed": true, "id": id })
    } else {
        json!({ "completed": false, "id": id, "detail": NOT_OWNED_DETAIL })
    }
    .to_string())
}

/// Mark a task THIS member claimed as FAILED (terminal failure) with `reason` — the `ensemble_fail`
/// counterpart of [`tool_complete`], same required fields and same ownership guard
/// (`ledger::fail_owned`). Returns `{failed:true, id}` or `{failed:false, id, detail}`.
fn tool_fail(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !args.is_object() {
        return Err(RpcError::invalid_params(
            "arguments must be an object with `id` and `reason`",
        ));
    }
    let id = required_str(args, "id")?;
    let reason = required_str(args, "reason")?;
    let failed = open_ledger(&ctx.repo)?
        .fail_owned(id, &ctx.name, reason, now_secs())
        .map_err(|e| RpcError::internal(format!("ledger fail: {e}")))?;
    Ok(if failed {
        json!({ "failed": true, "id": id })
    } else {
        json!({ "failed": false, "id": id, "detail": NOT_OWNED_DETAIL })
    }
    .to_string())
}

/// Delegate `task` to a HEADLESS governed crew sub-run in this repo via the injected `CrewRunner`
/// (which wraps `Conductor::run_in_repo`: implementer → test gate → reviewers → gate, in its own
/// throwaway worktree). `task` is a required non-blank string — validated BEFORE the runner is
/// touched, so a malformed call never starts a run. The call BLOCKS this request thread until the
/// sub-run reaches a terminal decision; because the MCP server runs each request on its own thread, a
/// member's concurrent quick tool calls (e.g. board polls) still flow meanwhile. The run executes in
/// THIS server's repo (`ctx.repo`), never a client-supplied path. Returns `{landed:true, rounds,
/// branch}` (land the kept branch with `ensemble_merge`) or `{landed:false, rounds, reason}`. A
/// server with no runner configured (only ever a unit-test `Ctx`; the binary always wires one) is a
/// -32603 internal condition — never a silent fake-land.
fn tool_run(args: &Value, ctx: &Ctx) -> Result<String, RpcError> {
    if !args.is_object() {
        return Err(RpcError::invalid_params(
            "arguments must be an object with `task`",
        ));
    }
    let task = required_str(args, "task")?;
    let runner = ctx.runner.as_ref().ok_or_else(|| {
        RpcError::internal("ensemble_run is not configured on this server (no crew runner)")
    })?;
    let s = runner.run(task, &ctx.repo);
    Ok(if s.landed {
        json!({ "landed": true, "rounds": s.rounds, "branch": s.branch })
    } else {
        json!({ "landed": false, "rounds": s.rounds, "reason": s.detail })
    }
    .to_string())
}

/// Turn one raw stdin line into the JSON-RPC response line to write, or `None` for a NOTIFICATION
/// (a request with no `id` member — never gets a response). Pure — the full request→response mapping
/// is testable without stdio.
///
/// JSON-RPC conformance: an unparseable line yields a `-32700` parse-error response with `id: null`
/// (so a confused client isn't left hanging); a message WITH an `id` member is a request even if the
/// id is `null`, so it always gets a response; only the absence of `id` makes it a notification.
pub fn handle_message(line: &str, ctx: &Ctx) -> Option<String> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(
                Value::Null,
                -32700,
                format!("parse error: {e}"),
            ))
        }
    };
    // No `id` member at all ⇒ notification (no response). `id: null` IS a request id (respond).
    let id = req.get("id")?.clone();
    // A request must carry a string `method`; a missing/non-string one is a malformed request.
    let method = match req.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return Some(error_response(
                id,
                -32600,
                "invalid request: missing or non-string method",
            ))
        }
    };
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let resp = match dispatch(method, &params, ctx) {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } })
        }
    };
    Some(resp.to_string())
}

/// A JSON-RPC error-response line for `id` (used for protocol-level errors like parse failures).
fn error_response(id: Value, code: i64, message: impl Into<String>) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
        .to_string()
}

/// Serve the MCP protocol over stdin/stdout until EOF. Each request runs on its own thread (so a long
/// tool call doesn't block a concurrent quick one — the handler computes its payload BEFORE taking the
/// stdout lock, which it holds only for the `writeln!`); responses are written under a stdout `Mutex`,
/// one complete line each, so concurrent responses never interleave (JSON-RPC pairs by id, so
/// out-of-order is legal). A counting semaphore caps concurrency at MAX_INFLIGHT: the "doesn't block"
/// guarantee holds BELOW saturation — with that many already-stuck long calls the reader blocks on
/// `acquire` (intended backpressure). Each request holds an RAII permit (released even on a handler
/// panic). Finished threads are reaped each iteration (the handle vec stays bounded to in-flight
/// requests); in-flight threads are JOINED at EOF so their responses flush before we exit (else a
/// piped batch could lose the responses to its last requests). Blocks until stdin closes.
pub fn serve_stdio(ctx: Ctx) -> std::io::Result<()> {
    let ctx = Arc::new(ctx);
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let sem = Arc::new(Semaphore::new(MAX_INFLIGHT));
    let stdin = std::io::stdin();
    let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // Backpressure: block until an in-flight slot frees. The RAII permit returns its slot on the
        // thread's normal end, a panic (Drop runs during unwind), OR if `work` is dropped unspawned.
        let permit = sem.acquire();
        let ctx = ctx.clone();
        let out = out.clone();
        let work = move || {
            let _permit = permit; // released on drop — normal, panic, or unspawned
            if let Some(resp) = handle_message(&line, &ctx) {
                // Recover from a poisoned stdout mutex (a prior panic-while-writing) so later
                // responses still flush, rather than being silently dropped forever.
                let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
                let _ = writeln!(o, "{resp}");
                let _ = o.flush();
            }
        };
        // `Builder::spawn` returns a Result (it does NOT panic like `thread::spawn`). On the rare
        // resource-exhaustion failure the moved `work` is dropped, and its permit's Drop releases the
        // slot, so we just drop the request (the client can retry); concurrency stays bounded.
        match std::thread::Builder::new().spawn(work) {
            Ok(h) => handles.push(h),
            Err(e) => eprintln!("ensemble mcp: thread spawn failed, dropping request: {e}"),
        }
        handles.retain(|h| !h.is_finished()); // reap completed threads; keep the vec in-flight-sized
    }
    for h in handles {
        let _ = h.join(); // flush the last in-flight responses before exiting
    }
    Ok(())
}

#[cfg(test)]
impl Semaphore {
    fn available(&self) -> usize {
        *self.permits.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permit_is_released_even_on_handler_panic() {
        // the RAII guard must return the permit during unwind — else MAX_INFLIGHT panics wedge serve.
        let sem = Arc::new(Semaphore::new(2));
        assert_eq!(sem.available(), 2);
        let s = sem.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _permit = s.acquire(); // available → 1
            panic!("handler boom"); // unwind drops _permit → release
        }));
        assert_eq!(
            sem.available(),
            2,
            "permit returned during unwind, not leaked"
        );
    }

    fn ctx(repo: &std::path::Path) -> Ctx {
        Ctx {
            repo: repo.to_path_buf(),
            name: "tester".into(),
            team: "default".into(),
            runner: None,
            supervisor: None,
        }
    }

    fn call(line: &str, ctx: &Ctx) -> Option<Value> {
        super::handle_message(line, ctx).map(|s| serde_json::from_str(&s).unwrap())
    }

    fn tool_payload(r: &Value) -> Value {
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn initialize_echoes_protocol_and_advertises_tools_capability() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(
            r["result"]["protocolVersion"], "2025-03-26",
            "echoes the client's version"
        );
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "ensemble");
    }

    #[test]
    fn tools_list_includes_mesh_and_board_read() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ensemble_mesh"));
        assert!(names.contains(&"ensemble_board_read"));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":3,"method":"bogus/thing"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn request_with_missing_method_is_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(r#"{"jsonrpc":"2.0","id":7}"#, &ctx(tmp.path())).unwrap();
        assert_eq!(
            r["error"]["code"], -32600,
            "a request with no method is -32600 Invalid Request"
        );
    }

    #[test]
    fn notification_without_id_yields_no_response() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(call(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &ctx(tmp.path())
        )
        .is_none());
    }

    #[test]
    fn unparseable_line_returns_a_parse_error_with_null_id() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call("this is not json", &ctx(tmp.path())).unwrap();
        assert_eq!(r["error"]["code"], -32700);
        assert!(r["id"].is_null(), "a parse error carries a null id");
    }

    #[test]
    fn id_null_is_a_request_not_a_notification() {
        // a message WITH an id member (even null) is a request → gets a response; only a MISSING id
        // is a notification.
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert!(
            r["result"]["tools"].is_array(),
            "id:null still gets a response"
        );
    }

    #[test]
    fn board_read_rejects_a_non_integer_since() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"ensemble_board_read","arguments":{"since":"oops"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(
            r["error"]["code"], -32602,
            "a bad `since` is invalid params, not a silent reset"
        );
    }

    #[test]
    fn board_read_tool_returns_posted_messages_and_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let board = FileBoard::open(tmp.path());
        board.post("codex", "result", "did the thing").unwrap();
        board.post("claude", "verdict", "VERDICT: LGTM").unwrap();

        let r = call(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"ensemble_board_read","arguments":{"since":1}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        // the tool result text is a JSON payload {messages, next}
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["next"], 2, "cursor is the new total");
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "since=1 skips the first message");
        assert_eq!(msgs[0]["index"], 1);
        assert_eq!(msgs[0]["from"], "claude");
        assert!(text.contains("VERDICT: LGTM"));
    }

    #[test]
    fn tools_call_unknown_tool_is_invalid_params() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn tools_list_includes_board_post_requiring_kind_and_body() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":20,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        let post = tools
            .iter()
            .find(|t| t["name"] == "ensemble_board_post")
            .expect("board_post is listed as a tool");
        let req = post["inputSchema"]["required"].as_array().unwrap();
        assert!(
            req.iter().any(|v| v == "kind") && req.iter().any(|v| v == "body"),
            "board_post declares kind+body required: {req:?}"
        );
    }

    #[test]
    fn tools_list_includes_team_and_control_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":80,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for name in [
            "ensemble_team_status",
            "ensemble_team_say",
            "ensemble_team_inbox",
            "ensemble_watch",
            "ensemble_steer",
            "ensemble_abort",
        ] {
            assert!(names.contains(&name), "{name} should be advertised");
        }
    }

    #[test]
    fn team_say_posts_as_this_member_and_inbox_reads_it() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let said = call(
            r#"{"jsonrpc":"2.0","id":81,"method":"tools/call","params":{"name":"ensemble_team_say","arguments":{"body":"hello team"}}}"#,
            &c,
        )
        .unwrap();
        let p = tool_payload(&said);
        assert_eq!(p["posted"], true);
        assert_eq!(p["next"], 1);

        let inbox = call(
            r#"{"jsonrpc":"2.0","id":82,"method":"tools/call","params":{"name":"ensemble_team_inbox","arguments":{"since":0}}}"#,
            &c,
        )
        .unwrap();
        let p = tool_payload(&inbox);
        assert_eq!(p["next"], 1);
        assert_eq!(
            p["messages"][0]["from"], "tester",
            "server identity wins over client `from`"
        );
        assert_eq!(p["messages"][0]["kind"], "note");
        assert_eq!(p["messages"][0]["body"], "hello team");
    }

    #[test]
    fn team_say_rejects_client_supplied_author_field() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":811,"method":"tools/call","params":{"name":"ensemble_team_say","arguments":{"body":"hello team","from":"mallory"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("from"));
        assert!(
            crate::FileBoard::open_at(&tmp.path().join(".ensemble"))
                .read_since(0)
                .unwrap()
                .is_empty(),
            "unknown author fields are rejected before any post"
        );
    }

    #[test]
    fn team_tools_can_target_a_named_team_without_touching_default_board() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        call(
            r#"{"jsonrpc":"2.0","id":83,"method":"tools/call","params":{"name":"ensemble_team_say","arguments":{"team":"ops","kind":"plan","body":"ops online"}}}"#,
            &c,
        )
        .unwrap();

        let ops = call(
            r#"{"jsonrpc":"2.0","id":84,"method":"tools/call","params":{"name":"ensemble_team_status","arguments":{"team":"ops"}}}"#,
            &c,
        )
        .unwrap();
        let ops = tool_payload(&ops);
        assert_eq!(ops["team"], "ops");
        assert_eq!(ops["boardLen"], 1);

        let default = call(
            r#"{"jsonrpc":"2.0","id":85,"method":"tools/call","params":{"name":"ensemble_team_status"}}"#,
            &c,
        )
        .unwrap();
        let default = tool_payload(&default);
        assert_eq!(default["team"], "default");
        assert_eq!(
            default["boardLen"], 0,
            "named team writes must not leak to default board"
        );
    }

    #[test]
    fn team_inbox_rejects_a_bad_since_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":86,"method":"tools/call","params":{"name":"ensemble_team_inbox","arguments":{"since":"bad"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("since"));
        assert!(
            !tmp.path().join(".ensemble").exists(),
            "validation failure should not create state"
        );
    }

    #[test]
    fn watch_returns_bounded_stream_lines_with_a_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let feed = crate::Feed::open(crate::member_stream_path(tmp.path(), "run-1"));
        feed.append(r#"{"ev":"msg","body":"first"}"#).unwrap();
        feed.append(r#"{"ev":"msg","body":"second"}"#).unwrap();

        let r = call(
            r#"{"jsonrpc":"2.0","id":87,"method":"tools/call","params":{"name":"ensemble_watch","arguments":{"name":"run-1","since":0,"limit":1}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let p = tool_payload(&r);
        assert_eq!(p["name"], "run-1");
        assert_eq!(p["next"], 1);
        assert_eq!(p["messages"].as_array().unwrap().len(), 1);
        assert_eq!(p["messages"][0]["index"], 0);
        assert_eq!(p["messages"][0]["event"]["body"], "first");
    }

    #[test]
    fn steer_and_abort_append_control_commands_as_this_member() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let steer = call(
            r#"{"jsonrpc":"2.0","id":88,"method":"tools/call","params":{"name":"ensemble_steer","arguments":{"name":"run-2","prompt":"stay focused"}}}"#,
            &c,
        )
        .unwrap();
        assert_eq!(tool_payload(&steer)["next"], 1);
        let abort = call(
            r#"{"jsonrpc":"2.0","id":89,"method":"tools/call","params":{"name":"ensemble_abort","arguments":{"name":"run-2","hard":true}}}"#,
            &c,
        )
        .unwrap();
        assert_eq!(tool_payload(&abort)["next"], 2);

        let lines = crate::Feed::open(crate::member_control_path(tmp.path(), "run-2"))
            .read_since(0)
            .unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<crate::ControlCmd>(&lines[0]).unwrap(),
            crate::ControlCmd::Steer {
                from: "tester".into(),
                prompt: "stay focused".into(),
            }
        );
        assert_eq!(
            serde_json::from_str::<crate::ControlCmd>(&lines[1]).unwrap(),
            crate::ControlCmd::Abort {
                from: "tester".into(),
                hard: true,
            }
        );
    }

    #[test]
    fn watch_and_control_tools_use_the_same_feeds_as_the_cli_even_for_named_server_teams() {
        let tmp = tempfile::tempdir().unwrap();
        let c = Ctx {
            repo: tmp.path().to_path_buf(),
            name: "tester".into(),
            team: "ops".into(),
            runner: None,
            supervisor: None,
        };
        crate::Feed::open(crate::member_stream_path(tmp.path(), "run-ops"))
            .append(r#"{"ev":"msg","body":"cli stream"}"#)
            .unwrap();

        let watched = call(
            r#"{"jsonrpc":"2.0","id":881,"method":"tools/call","params":{"name":"ensemble_watch","arguments":{"name":"run-ops","since":0,"limit":10}}}"#,
            &c,
        )
        .unwrap();
        let watched = tool_payload(&watched);
        assert_eq!(watched["messages"][0]["event"]["body"], "cli stream");
        assert!(
            !tmp.path()
                .join(".ensemble")
                .join("teams")
                .join("ops")
                .join("stream")
                .join("run-ops.ndjson")
                .exists(),
            "watch must not silently switch to a team-scoped stream feed before runs are team-aware"
        );

        call(
            r#"{"jsonrpc":"2.0","id":882,"method":"tools/call","params":{"name":"ensemble_steer","arguments":{"name":"run-ops","prompt":"same feed"}}}"#,
            &c,
        )
        .unwrap();
        let lines = crate::Feed::open(crate::member_control_path(tmp.path(), "run-ops"))
            .read_since(0)
            .unwrap();
        assert_eq!(lines.len(), 1, "MCP steer writes to the CLI control feed");
        assert!(
            !tmp.path()
                .join(".ensemble")
                .join("teams")
                .join("ops")
                .join("control")
                .join("run-ops.ndjson")
                .exists(),
            "steer must not silently switch to a team-scoped control feed before runs are team-aware"
        );
    }

    #[test]
    fn control_tools_reject_ambiguous_target_names() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["../run", r"ops\run", "..", "."] {
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":883,"method":"tools/call","params":{{"name":"ensemble_abort","arguments":{{"name":"{name}"}}}}}}"#
            );
            let r = call(&body, &ctx(tmp.path())).unwrap();
            assert_eq!(r["error"]["code"], -32602, "{name} should be rejected");
        }
    }

    #[test]
    fn steer_requires_a_prompt_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":90,"method":"tools/call","params":{"name":"ensemble_steer","arguments":{"name":"run-3"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("prompt"));
        assert!(
            crate::Feed::open(crate::member_control_path(tmp.path(), "run-3"))
                .read_since(0)
                .unwrap()
                .is_empty(),
            "validation happens before appending control"
        );
    }

    #[test]
    fn board_post_tool_appends_under_this_members_name() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"ensemble_board_post","arguments":{"kind":"result","body":"shipped the parser"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["posted"], true);
        assert_eq!(
            payload["next"], 1,
            "the board length after the post is the new cursor"
        );
        // the message actually landed on the shared board, attributed to ctx.name (NOT a client field)
        let posted = FileBoard::open(tmp.path()).read_since(0).unwrap();
        assert_eq!(posted.len(), 1);
        assert_eq!(
            posted[0].from, "tester",
            "attributed to this member, not client-supplied"
        );
        assert_eq!(posted[0].kind, "result");
        assert_eq!(posted[0].body, "shipped the parser");
    }

    #[test]
    fn board_post_then_board_read_roundtrip_through_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        call(
            r#"{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"ensemble_board_post","arguments":{"kind":"question","body":"anyone on auth?"}}}"#,
            &c,
        )
        .unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"ensemble_board_read"}}"#,
            &c,
        )
        .unwrap();
        let payload: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["next"], 1);
        assert_eq!(payload["messages"][0]["body"], "anyone on auth?");
        assert_eq!(payload["messages"][0]["from"], "tester");
    }

    #[test]
    fn board_post_requires_a_body() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":24,"method":"tools/call","params":{"name":"ensemble_board_post","arguments":{"kind":"result"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        let msg = r["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("body"),
            "names the missing field, not 'unknown tool': {msg}"
        );
    }

    #[test]
    fn board_post_requires_a_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":25,"method":"tools/call","params":{"name":"ensemble_board_post","arguments":{"body":"orphan"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("kind"));
    }

    #[test]
    fn board_post_rejects_a_non_string_body() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":26,"method":"tools/call","params":{"name":"ensemble_board_post","arguments":{"kind":"result","body":123}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("body"));
    }

    #[test]
    fn board_post_rejects_a_blank_body_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":27,"method":"tools/call","params":{"name":"ensemble_board_post","arguments":{"kind":"result","body":"   "}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(
            r["error"]["code"], -32602,
            "a blank body is a client error, not a silent empty post"
        );
        assert!(
            FileBoard::open(tmp.path()).is_empty().unwrap(),
            "validation happens BEFORE the post, so nothing is written"
        );
    }

    /// Initialize a real git repo with one commit (the worktree tool needs a HEAD to branch from).
    fn git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?} failed"
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        run(&["branch", "-M", "main"]); // deterministic default-branch name (merge tests target it)
    }

    /// Run a git command in `dir`, asserting success (for building merge-test fixtures).
    fn git_ok(dir: &std::path::Path, args: &[&str]) {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap()
                .status
                .success(),
            "git {args:?} failed"
        );
    }

    #[test]
    fn tools_list_includes_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":30,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ensemble_worktree"));
    }

    #[test]
    fn worktree_tool_creates_a_persistent_worktree_for_this_member() {
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let r = call(
            r#"{"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"ensemble_worktree","arguments":{"task":"feature-x"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let payload: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["branch"], "ensemble/tester/feature-x",
            "branch carries the member"
        );
        assert_eq!(payload["slug"], "tester/feature-x");
        let path = payload["path"].as_str().unwrap();
        assert!(
            std::path::Path::new(path).exists(),
            "the worktree dir persists (not RAII-removed)"
        );
    }

    #[test]
    fn worktree_tool_is_idempotent_per_member_and_task() {
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let c = ctx(tmp.path());
        let one = call(
            r#"{"jsonrpc":"2.0","id":32,"method":"tools/call","params":{"name":"ensemble_worktree","arguments":{"task":"x"}}}"#,
            &c,
        )
        .unwrap();
        let two = call(
            r#"{"jsonrpc":"2.0","id":33,"method":"tools/call","params":{"name":"ensemble_worktree","arguments":{"task":"x"}}}"#,
            &c,
        )
        .unwrap();
        let p1: Value =
            serde_json::from_str(one["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let p2: Value =
            serde_json::from_str(two["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            p1["path"], p2["path"],
            "same member+task re-attaches to the same worktree"
        );
    }

    #[test]
    fn worktree_tool_defaults_task_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let r = call(
            r#"{"jsonrpc":"2.0","id":34,"method":"tools/call","params":{"name":"ensemble_worktree"}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let payload: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["slug"], "tester/work",
            "an absent task defaults to 'work'"
        );
    }

    #[test]
    fn worktree_tool_treats_null_task_as_absent() {
        // consistent with ensemble_board_read's optional `since` (null == not provided -> default).
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let r = call(
            r#"{"jsonrpc":"2.0","id":36,"method":"tools/call","params":{"name":"ensemble_worktree","arguments":{"task":null}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let payload: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["slug"], "tester/work",
            "a null task defaults to 'work'"
        );
    }

    #[test]
    fn worktree_tool_rejects_a_blank_task() {
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let r = call(
            r#"{"jsonrpc":"2.0","id":35,"method":"tools/call","params":{"name":"ensemble_worktree","arguments":{"task":"  "}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("task"));
    }

    #[test]
    fn tools_list_includes_enqueue_and_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":40,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ensemble_enqueue"));
        assert!(names.contains(&"ensemble_claim"));
    }

    #[test]
    fn enqueue_then_claim_roundtrips_a_task() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let e = call(
            r#"{"jsonrpc":"2.0","id":41,"method":"tools/call","params":{"name":"ensemble_enqueue","arguments":{"descr":"port the parser"}}}"#,
            &c,
        )
        .unwrap();
        let ep: Value =
            serde_json::from_str(e["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(ep["enqueued"], true);
        let id = ep["id"].as_str().unwrap().to_string();

        let cl = call(
            r#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"ensemble_claim"}}"#,
            &c,
        )
        .unwrap();
        let cp: Value =
            serde_json::from_str(cl["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(cp["claimed"], true);
        assert_eq!(cp["descr"], "port the parser");
        assert_eq!(cp["id"], id, "claimed the very task we enqueued");
    }

    #[test]
    fn enqueue_is_idempotent_on_descr() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let body = r#"{"jsonrpc":"2.0","id":43,"method":"tools/call","params":{"name":"ensemble_enqueue","arguments":{"descr":"same task"}}}"#;
        let first: Value = serde_json::from_str(
            call(body, &c).unwrap()["result"]["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let second: Value = serde_json::from_str(
            call(body, &c).unwrap()["result"]["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first["enqueued"], true);
        assert_eq!(
            second["enqueued"], false,
            "the same descr is a no-op (stable-hash id)"
        );
        assert_eq!(first["id"], second["id"]);
    }

    #[test]
    fn claim_on_an_empty_queue_is_not_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":44,"method":"tools/call","params":{"name":"ensemble_claim"}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            p["claimed"], false,
            "an empty queue is a normal result, not an error"
        );
    }

    #[test]
    fn claim_attributes_the_task_to_this_member() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path()); // name = "tester"
        call(
            r#"{"jsonrpc":"2.0","id":45,"method":"tools/call","params":{"name":"ensemble_enqueue","arguments":{"descr":"x"}}}"#,
            &c,
        )
        .unwrap();
        call(
            r#"{"jsonrpc":"2.0","id":46,"method":"tools/call","params":{"name":"ensemble_claim"}}"#,
            &c,
        )
        .unwrap();
        // the claim is recorded under THIS member's identity (ctx.name), not a client-supplied worker
        let l =
            crate::ledger::Ledger::open(&tmp.path().join(".ensemble").join("ledger.db")).unwrap();
        let t = l
            .list()
            .unwrap()
            .into_iter()
            .find(|t| t.descr == "x")
            .unwrap();
        assert_eq!(t.claimed_by.as_deref(), Some("tester"));
    }

    #[test]
    fn enqueue_requires_a_descr() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":47,"method":"tools/call","params":{"name":"ensemble_enqueue","arguments":{}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(
            r["error"]["message"].as_str().unwrap().contains("descr"),
            "names the field, not 'unknown tool'"
        );
    }

    #[test]
    fn tools_list_includes_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":50,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ensemble_merge"));
    }

    #[test]
    fn merge_tool_lands_a_clean_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_repo(repo);
        // a branch that only ADDS a file → merges cleanly into main
        git_ok(repo, &["checkout", "-q", "-b", "ensemble/feat"]);
        std::fs::write(repo.join("new.txt"), "hello").unwrap();
        git_ok(repo, &["add", "."]);
        git_ok(repo, &["commit", "-q", "-m", "feat"]);
        git_ok(repo, &["checkout", "-q", "main"]);

        let r = call(
            r#"{"jsonrpc":"2.0","id":51,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{"branch":"ensemble/feat","into":"main"}}}"#,
            &ctx(repo),
        )
        .unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(p["landed"], true);
        assert_eq!(p["branch"], "ensemble/feat");
        assert_eq!(p["into"], "main");
        assert!(
            repo.join("new.txt").exists(),
            "the branch's file is now on main's worktree"
        );
    }

    #[test]
    fn merge_tool_reports_a_conflict_without_landing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_repo(repo); // main: f="x"
        git_ok(repo, &["checkout", "-q", "-b", "ensemble/conf"]);
        std::fs::write(repo.join("f"), "branch-edit").unwrap();
        git_ok(repo, &["commit", "-q", "-am", "branch"]);
        git_ok(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("f"), "main-edit").unwrap();
        git_ok(repo, &["commit", "-q", "-am", "main"]);

        // `into` omitted → defaults to "main"
        let r = call(
            r#"{"jsonrpc":"2.0","id":52,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{"branch":"ensemble/conf"}}}"#,
            &ctx(repo),
        )
        .unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            p["landed"], false,
            "a conflict is a reported outcome, not an error"
        );
        assert!(
            p["conflict"].as_array().unwrap().iter().any(|v| v == "f"),
            "names the conflicting path: {p}"
        );
        // the merge was aborted and main restored to its own edit (clean tree)
        assert_eq!(
            std::fs::read_to_string(repo.join("f")).unwrap(),
            "main-edit"
        );
    }

    #[test]
    fn merge_requires_a_branch() {
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let r = call(
            r#"{"jsonrpc":"2.0","id":53,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(
            r["error"]["message"].as_str().unwrap().contains("branch"),
            "names the field, not 'unknown tool'"
        );
    }

    #[test]
    fn merge_rejects_a_flag_like_branch_to_block_git_option_injection() {
        let tmp = tempfile::tempdir().unwrap();
        git_repo(tmp.path());
        let r = call(
            r#"{"jsonrpc":"2.0","id":54,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{"branch":"--upload-pack=x"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(
            r["error"]["message"].as_str().unwrap().contains("branch"),
            "rejects a '-'-leading ref: {}",
            r["error"]["message"]
        );
    }

    #[test]
    fn merge_rejects_an_into_that_is_a_path_not_a_branch() {
        // Regression (codex gate, slice 4a): `into:"f"` (a tracked FILE, not a branch) must be
        // rejected — else `git checkout f` does a PATH checkout and the merge lands on the WRONG
        // branch while reporting success.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_repo(repo); // "f" is a tracked file, NOT a branch
        git_ok(repo, &["checkout", "-q", "-b", "ensemble/x"]);
        std::fs::write(repo.join("n.txt"), "y").unwrap();
        git_ok(repo, &["add", "."]);
        git_ok(repo, &["commit", "-q", "-m", "x"]);
        git_ok(repo, &["checkout", "-q", "main"]);
        let r = call(
            r#"{"jsonrpc":"2.0","id":55,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{"branch":"ensemble/x","into":"f"}}}"#,
            &ctx(repo),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a local branch"));
    }

    #[test]
    fn merge_rejects_into_head_pseudoref() {
        // Regression (codex gate, slice 4a r3): `into:"HEAD"` must be rejected — `git rev-parse
        // --symbolic-full-name HEAD` resolves to the CURRENT branch, not `refs/heads/HEAD`, so the raw
        // name can't unambiguously mean a local branch (existence alone wouldn't catch this).
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_repo(repo);
        git_ok(repo, &["checkout", "-q", "-b", "ensemble/x"]);
        std::fs::write(repo.join("n.txt"), "y").unwrap();
        git_ok(repo, &["add", "."]);
        git_ok(repo, &["commit", "-q", "-m", "x"]);
        git_ok(repo, &["checkout", "-q", "main"]);
        let r = call(
            r#"{"jsonrpc":"2.0","id":57,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{"branch":"ensemble/x","into":"HEAD"}}}"#,
            &ctx(repo),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a local branch"));
    }

    #[test]
    fn merge_rejects_a_nonexistent_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_repo(repo);
        let r = call(
            r#"{"jsonrpc":"2.0","id":56,"method":"tools/call","params":{"name":"ensemble_merge","arguments":{"branch":"ensemble/ghost"}}}"#,
            &ctx(repo),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a local branch"));
    }

    #[test]
    fn concurrent_merges_serialize_and_both_land() {
        // ensemble_merge mutates the MAIN worktree (checkout into + merge); the per-repo lock must
        // serialize concurrent merges (the MCP server runs requests on parallel threads) so they don't
        // race git's index. Two non-conflicting branches must BOTH land.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_repo(repo);
        for (b, f) in [("ensemble/a", "a.txt"), ("ensemble/b", "b.txt")] {
            git_ok(repo, &["checkout", "-q", "-b", b, "main"]);
            std::fs::write(repo.join(f), "x").unwrap();
            git_ok(repo, &["add", "."]);
            git_ok(repo, &["commit", "-q", "-m", b]);
        }
        git_ok(repo, &["checkout", "-q", "main"]);
        let c = ctx(repo);
        std::thread::scope(|s| {
            for b in ["ensemble/a", "ensemble/b"] {
                let c = &c;
                s.spawn(move || {
                    let body = format!(
                        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"ensemble_merge","arguments":{{"branch":"{b}"}}}}}}"#
                    );
                    let _ = call(&body, c);
                });
            }
        });
        git_ok(repo, &["checkout", "-q", "main"]);
        assert!(
            repo.join("a.txt").exists() && repo.join("b.txt").exists(),
            "both branches landed under the serializing lock (no clobber)"
        );
    }

    #[test]
    fn tools_list_includes_complete_and_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":58,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ensemble_complete"));
        assert!(names.contains(&"ensemble_fail"));
    }

    #[test]
    fn complete_marks_a_claimed_task_done() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path()); // name = "tester"
                                 // enqueue + claim the task AS tester, through the tools
        let e = call(
            r#"{"jsonrpc":"2.0","id":61,"method":"tools/call","params":{"name":"ensemble_enqueue","arguments":{"descr":"ship it"}}}"#,
            &c,
        )
        .unwrap();
        let id = {
            let ep: Value =
                serde_json::from_str(e["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            ep["id"].as_str().unwrap().to_string()
        };
        call(
            r#"{"jsonrpc":"2.0","id":62,"method":"tools/call","params":{"name":"ensemble_claim"}}"#,
            &c,
        )
        .unwrap();
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":63,"method":"tools/call","params":{{"name":"ensemble_complete","arguments":{{"id":"{id}","outcome":"LANDED ensemble/tester/x"}}}}}}"#
        );
        let r = call(&body, &c).unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(p["completed"], true);
        assert_eq!(p["id"], id);
        // a terminal DONE record with our outcome actually landed in the shared ledger
        let l =
            crate::ledger::Ledger::open(&tmp.path().join(".ensemble").join("ledger.db")).unwrap();
        let t = l.list().unwrap().into_iter().find(|t| t.id == id).unwrap();
        assert_eq!(t.state_str(), "done");
        assert_eq!(t.outcome.as_deref(), Some("LANDED ensemble/tester/x"));
    }

    #[test]
    fn complete_rejects_a_task_claimed_by_another_member() {
        // ownership guard: a member can only complete a task IT claimed (the anti-impersonation theme —
        // like board posts/claims being attributed to ctx.name, never a client field).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".ensemble");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("ledger.db");
        {
            let mut l = crate::ledger::Ledger::open(&db).unwrap();
            l.enqueue("t1", "shared task", 1).unwrap();
            l.claim("other-member", 10).unwrap(); // someone ELSE owns it
        }
        // tester (ctx.name) never claimed t1 → cannot complete it
        let r = call(
            r#"{"jsonrpc":"2.0","id":64,"method":"tools/call","params":{"name":"ensemble_complete","arguments":{"id":"t1","outcome":"sneaky"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            p["completed"], false,
            "can't complete another member's task"
        );
        assert!(p["detail"].is_string(), "explains why it didn't take: {p}");
        // the task is untouched: still claimed by the real owner, no outcome written
        let l = crate::ledger::Ledger::open(&db).unwrap();
        let t = l
            .list()
            .unwrap()
            .into_iter()
            .find(|t| t.id == "t1")
            .unwrap();
        assert_eq!(t.state_str(), "claimed");
        assert_eq!(t.claimed_by.as_deref(), Some("other-member"));
        assert_eq!(t.outcome, None);
    }

    #[test]
    fn complete_requires_id_and_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":65,"method":"tools/call","params":{"name":"ensemble_complete","arguments":{"id":"x"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(
            r["error"]["message"].as_str().unwrap().contains("outcome"),
            "names the missing field"
        );
    }

    #[test]
    fn fail_marks_a_claimed_task_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let e = call(
            r#"{"jsonrpc":"2.0","id":66,"method":"tools/call","params":{"name":"ensemble_enqueue","arguments":{"descr":"flaky thing"}}}"#,
            &c,
        )
        .unwrap();
        let id = {
            let ep: Value =
                serde_json::from_str(e["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            ep["id"].as_str().unwrap().to_string()
        };
        call(
            r#"{"jsonrpc":"2.0","id":67,"method":"tools/call","params":{"name":"ensemble_claim"}}"#,
            &c,
        )
        .unwrap();
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":68,"method":"tools/call","params":{{"name":"ensemble_fail","arguments":{{"id":"{id}","reason":"ESCALATED: tests never passed"}}}}}}"#
        );
        let r = call(&body, &c).unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(p["failed"], true);
        let l =
            crate::ledger::Ledger::open(&tmp.path().join(".ensemble").join("ledger.db")).unwrap();
        let t = l.list().unwrap().into_iter().find(|t| t.id == id).unwrap();
        assert_eq!(t.state_str(), "failed");
        assert_eq!(t.outcome.as_deref(), Some("ESCALATED: tests never passed"));
    }

    /// A fake `CrewRunner` that records the (task, repo) it was handed and returns a canned summary —
    /// so `ensemble_run`'s happy path is hermetic (no real CLI crew is ever spawned in a unit test).
    struct FakeRunner {
        seen: std::sync::Mutex<Option<(String, std::path::PathBuf)>>,
        summary: RunSummary,
    }
    impl CrewRunner for FakeRunner {
        fn run(&self, task: &str, repo: &std::path::Path) -> RunSummary {
            *self.seen.lock().unwrap() = Some((task.to_string(), repo.to_path_buf()));
            self.summary.clone()
        }
    }

    fn ctx_with_runner(repo: &std::path::Path, runner: Arc<dyn CrewRunner>) -> Ctx {
        Ctx {
            repo: repo.to_path_buf(),
            name: "tester".into(),
            team: "default".into(),
            runner: Some(runner),
            supervisor: None,
        }
    }

    struct FakeSupervisor {
        seen: std::sync::Mutex<Option<(SuperviseRequest, std::path::PathBuf, String)>>,
        summary: SuperviseSummary,
    }
    impl SupervisorRunner for FakeSupervisor {
        fn supervise(
            &self,
            req: SuperviseRequest,
            repo: &std::path::Path,
            caller: &str,
        ) -> Result<SuperviseSummary, String> {
            *self.seen.lock().unwrap() = Some((req, repo.to_path_buf(), caller.to_string()));
            Ok(self.summary.clone())
        }
    }

    fn ctx_with_supervisor(repo: &std::path::Path, supervisor: Arc<dyn SupervisorRunner>) -> Ctx {
        Ctx {
            repo: repo.to_path_buf(),
            name: "tester".into(),
            team: "default".into(),
            runner: None,
            supervisor: Some(supervisor),
        }
    }

    fn tool_names(r: &Value) -> Vec<String> {
        r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn tools_list_advertises_run_only_when_a_runner_is_configured() {
        let tmp = tempfile::tempdir().unwrap();
        // no runner → ensemble_run is NOT advertised: tools/list is a capability contract and must not
        // promise a tool a call would reject with -32603 (codex gate, slice 4b-ii).
        let bare = call(
            r#"{"jsonrpc":"2.0","id":70,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert!(
            !tool_names(&bare).iter().any(|n| n == "ensemble_run"),
            "an unconfigured run must not be advertised"
        );
        // …but the OTHER tools are always present
        assert!(tool_names(&bare).iter().any(|n| n == "ensemble_merge"));
        // with a runner wired → it IS advertised
        let fake = Arc::new(FakeRunner {
            seen: std::sync::Mutex::new(None),
            summary: RunSummary {
                landed: true,
                rounds: 1,
                branch: None,
                detail: String::new(),
            },
        });
        let wired = call(
            r#"{"jsonrpc":"2.0","id":70,"method":"tools/list"}"#,
            &ctx_with_runner(tmp.path(), fake),
        )
        .unwrap();
        assert!(
            tool_names(&wired).iter().any(|n| n == "ensemble_run"),
            "a configured runner advertises ensemble_run"
        );
    }

    #[test]
    fn tools_list_advertises_supervise_only_when_supervisor_is_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = call(
            r#"{"jsonrpc":"2.0","id":75,"method":"tools/list"}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert!(
            !tool_names(&bare).iter().any(|n| n == "ensemble_supervise"),
            "an unconfigured supervisor must not be advertised"
        );
        let fake = Arc::new(FakeSupervisor {
            seen: std::sync::Mutex::new(None),
            summary: SuperviseSummary {
                name: "run".into(),
                team: "default".into(),
                agent: "claude".into(),
                recommendation: crate::SupervisorRecommendation::OnTrack,
                reason: "ok".into(),
                steer: None,
                critical: false,
                board_next: 1,
                control_next: None,
            },
        });
        let wired = call(
            r#"{"jsonrpc":"2.0","id":76,"method":"tools/list"}"#,
            &ctx_with_supervisor(tmp.path(), fake),
        )
        .unwrap();
        assert!(
            tool_names(&wired).iter().any(|n| n == "ensemble_supervise"),
            "a configured supervisor advertises ensemble_supervise"
        );
    }

    #[test]
    fn supervise_tool_delegates_to_runner_and_shapes_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeSupervisor {
            seen: std::sync::Mutex::new(None),
            summary: SuperviseSummary {
                name: "run-1".into(),
                team: "ops".into(),
                agent: "codex".into(),
                recommendation: crate::SupervisorRecommendation::Steer,
                reason: "drifting".into(),
                steer: Some("focus on Task 6".into()),
                critical: false,
                board_next: 3,
                control_next: Some(1),
            },
        });
        let c = ctx_with_supervisor(tmp.path(), fake.clone());

        let r = call(
            r#"{"jsonrpc":"2.0","id":77,"method":"tools/call","params":{"name":"ensemble_supervise","arguments":{"name":"run-1","team":"ops","agent":"codex","since":4,"applySteer":true}}}"#,
            &c,
        )
        .unwrap();
        let p = tool_payload(&r);
        assert_eq!(p["recommendation"], "steer");
        assert_eq!(p["controlNext"], 1);

        let seen = fake.seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.0.name, "run-1");
        assert_eq!(seen.0.team.as_deref(), Some("ops"));
        assert_eq!(seen.0.agent, "codex");
        assert_eq!(seen.0.since, 4);
        assert!(seen.0.apply_steer);
        assert_eq!(seen.1, tmp.path());
        assert_eq!(seen.2, "tester");
    }

    #[test]
    fn run_delegates_to_the_crew_runner_and_shapes_a_landed_result() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeRunner {
            seen: std::sync::Mutex::new(None),
            summary: RunSummary {
                landed: true,
                rounds: 2,
                branch: Some("ensemble/x".into()),
                detail: String::new(),
            },
        });
        let c = ctx_with_runner(tmp.path(), fake.clone());
        let r = call(
            r#"{"jsonrpc":"2.0","id":71,"method":"tools/call","params":{"name":"ensemble_run","arguments":{"task":"refactor the parser"}}}"#,
            &c,
        )
        .unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(p["landed"], true);
        assert_eq!(p["rounds"], 2);
        assert_eq!(p["branch"], "ensemble/x");
        // the runner was handed the exact task + THIS server's repo (delegation runs in ctx.repo, never
        // a client-supplied path)
        let seen = fake.seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.0, "refactor the parser");
        assert_eq!(seen.1, tmp.path());
    }

    #[test]
    fn run_shapes_an_escalated_result_with_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeRunner {
            seen: std::sync::Mutex::new(None),
            summary: RunSummary {
                landed: false,
                rounds: 3,
                branch: None,
                detail: "tests never passed".into(),
            },
        });
        let c = ctx_with_runner(tmp.path(), fake);
        let r = call(
            r#"{"jsonrpc":"2.0","id":72,"method":"tools/call","params":{"name":"ensemble_run","arguments":{"task":"do X"}}}"#,
            &c,
        )
        .unwrap();
        let p: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(p["landed"], false);
        assert_eq!(p["rounds"], 3);
        assert_eq!(p["reason"], "tests never passed");
    }

    #[test]
    fn run_requires_a_task() {
        let tmp = tempfile::tempdir().unwrap();
        // even WITH a runner present, a missing task is a client error (-32602) — checked before the run
        let fake = Arc::new(FakeRunner {
            seen: std::sync::Mutex::new(None),
            summary: RunSummary {
                landed: true,
                rounds: 1,
                branch: None,
                detail: String::new(),
            },
        });
        let c = ctx_with_runner(tmp.path(), fake.clone());
        let r = call(
            r#"{"jsonrpc":"2.0","id":73,"method":"tools/call","params":{"name":"ensemble_run","arguments":{}}}"#,
            &c,
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains("task"));
        assert!(
            fake.seen.lock().unwrap().is_none(),
            "the runner was never invoked"
        );
    }

    #[test]
    fn run_without_a_configured_runner_is_internal_error() {
        // the unit-test Ctx has runner:None; the real binary always wires one. A valid call in that
        // state is a server-config condition (-32603), never a silent fake-land.
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":74,"method":"tools/call","params":{"name":"ensemble_run","arguments":{"task":"x"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32603);
    }
}
