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
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

/// The MCP protocol version we advertise when a client doesn't request one.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

/// Max in-flight request threads. A client could otherwise pipeline unboundedly and exhaust threads;
/// the reader loop blocks (backpressure) once this many are running.
const MAX_INFLIGHT: usize = 16;

/// A tiny counting semaphore (std only) to cap concurrent request handlers.
struct Semaphore {
    permits: Mutex<usize>,
    cv: Condvar,
}
impl Semaphore {
    fn new(n: usize) -> Self {
        Self { permits: Mutex::new(n), cv: Condvar::new() }
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

/// Per-server config: the repo (= crew session) and this member's identity for board posts.
pub struct Ctx {
    pub repo: PathBuf,
    pub name: String,
}

/// A JSON-RPC error object (code + message).
pub struct RpcError {
    pub code: i64,
    pub message: String,
}
impl RpcError {
    fn method_not_found(m: &str) -> Self {
        Self { code: -32601, message: format!("method not found: {m}") }
    }
    fn invalid_params(m: impl Into<String>) -> Self {
        Self { code: -32602, message: m.into() }
    }
    fn internal(m: impl Into<String>) -> Self {
        Self { code: -32603, message: m.into() }
    }
}

/// Route a JSON-RPC method to its result. Pure given `ctx` (no stdio) — the unit of the test suite.
pub fn dispatch(method: &str, params: &Value, ctx: &Ctx) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(initialize_result(params)),
        "tools/list" => Ok(tools_list()),
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

fn tools_list() -> Value {
    json!({ "tools": [
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
        }
    ]})
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
                Err(RpcError::invalid_params(format!("`{field}` must not be empty")))
            } else {
                Ok(s)
            }
        }
    }
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
        Err(e) => return Some(error_response(Value::Null, -32700, format!("parse error: {e}"))),
    };
    // No `id` member at all ⇒ notification (no response). `id: null` IS a request id (respond).
    let id = req.get("id")?.clone();
    // A request must carry a string `method`; a missing/non-string one is a malformed request.
    let method = match req.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return Some(error_response(id, -32600, "invalid request: missing or non-string method"))
        }
    };
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let resp = match dispatch(method, &params, ctx) {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } }),
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
        assert_eq!(sem.available(), 2, "permit returned during unwind, not leaked");
    }

    fn ctx(repo: &std::path::Path) -> Ctx {
        Ctx { repo: repo.to_path_buf(), name: "tester".into() }
    }

    fn call(line: &str, ctx: &Ctx) -> Option<Value> {
        super::handle_message(line, ctx).map(|s| serde_json::from_str(&s).unwrap())
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
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26", "echoes the client's version");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "ensemble");
    }

    #[test]
    fn tools_list_includes_mesh_and_board_read() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &ctx(tmp.path())).unwrap();
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
        let r = call(r#"{"jsonrpc":"2.0","id":3,"method":"bogus/thing"}"#, &ctx(tmp.path())).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn request_with_missing_method_is_invalid_request() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(r#"{"jsonrpc":"2.0","id":7}"#, &ctx(tmp.path())).unwrap();
        assert_eq!(r["error"]["code"], -32600, "a request with no method is -32600 Invalid Request");
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
        let r = call(r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#, &ctx(tmp.path())).unwrap();
        assert!(r["result"]["tools"].is_array(), "id:null still gets a response");
    }

    #[test]
    fn board_read_rejects_a_non_integer_since() {
        let tmp = tempfile::tempdir().unwrap();
        let r = call(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"ensemble_board_read","arguments":{"since":"oops"}}}"#,
            &ctx(tmp.path()),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602, "a bad `since` is invalid params, not a silent reset");
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
        let r = call(r#"{"jsonrpc":"2.0","id":20,"method":"tools/list"}"#, &ctx(tmp.path())).unwrap();
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
        assert_eq!(payload["next"], 1, "the board length after the post is the new cursor");
        // the message actually landed on the shared board, attributed to ctx.name (NOT a client field)
        let posted = FileBoard::open(tmp.path()).read_since(0).unwrap();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].from, "tester", "attributed to this member, not client-supplied");
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
        assert!(msg.contains("body"), "names the missing field, not 'unknown tool': {msg}");
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
        assert_eq!(r["error"]["code"], -32602, "a blank body is a client error, not a silent empty post");
        assert!(
            FileBoard::open(tmp.path()).is_empty().unwrap(),
            "validation happens BEFORE the post, so nothing is written"
        );
    }
}
