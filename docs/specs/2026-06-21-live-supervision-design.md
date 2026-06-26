# LIVE Cross-Session Supervision (0.5 督導主控台) — Design

**Status:** Approved skeleton, full design (this doc) pending operator review → then implementation plan.
**Date:** 2026-06-21
**Goal:** Let a main/operator session **observe, steer (inject ad-hoc prompts), and interrupt/abort** OTHER live AI-CLI sessions in real time — for all four fleet CLIs (claude, codex, opencode, agy), on **both Windows and macOS** — without weakening the existing double-gate landing governance.

This is the strongest form of the operator-flagged "必要" item: the `ensemble <cli>` wrap-launch idea (ensemble becomes the session's parent and grips it) unified with the shared-file control plane (cross-process / cross-machine transport). They are not a fork — they are two layers of one system.

---

## 1. Problem & user stories

A "session" here is one interactive AI-CLI run a human is using (e.g. `claude` in a terminal). Today ensemble drives CLIs two ways only: **headless one-shot** (`ExecAdapter`/`AgyAdapter`, whole-stdout-at-once) or **passive MCP server** (a client calls in; ensemble cannot preempt it). Neither lets a main session watch a *live interactive* session, push a prompt into it mid-flight, or interrupt it.

**US-1 — Wrap-launch (`ensemble <cli>`):** the operator types `ensemble claude` instead of `claude`. They use that terminal normally, but it is now a cluster-joined, remotely-steerable member: ensemble is its parent and grips it (PTY or structured API).

**US-2 — Remote supervise:** from the main session (possibly on another machine), `ensemble watch <member>` to see drift, `ensemble steer <member> "<prompt>"` to inject, `ensemble abort <member>` to interrupt — observed/applied against the live session from US-1.

**Why MCP alone can't do US-1/US-2's preempt:** an MCP client (claude started independently) is the *client*; ensemble is a passive *server* and cannot seize a client's turn. Real-time preempt requires owning the process — being its parent over a PTY, or driving its structured control channel.

---

## 2. Feasibility (the approved foundation)

Two mechanisms grip a session; the four CLIs split into two camps. Probed empirically on this machine (Windows, `COMPUTERNAME=YOYOGOOD`):

- **M-API (structured remote channel):** the CLI ships a server/daemon/attach surface driven over socket/HTTP/stdio. Clean observe/inject/interrupt, structured output (governance-friendly), and **never touches Windows ConPTY**.
- **M-PTY (raw PTY proxy):** universal fallback. `portable_pty` is already in-tree (`agy_adapter.rs`). Inject = write bytes to the PTY master; interrupt = write `0x03` / `kill_tree`. **macOS = clean Unix PTY; Windows = ConPTY hard edge** (`agy_adapter.rs:120-126` already documents a ConPTY cloned-reader EOF limitation; the robust path is a winpty-class backend).

| CLI | ver | interactive | M-API channel | M-PTY | Windows | macOS arm64 | camp |
|---|---|---|---|---|---|---|---|
| **codex** | 0.141.0 | `codex` TUI | ✅ `remote-control start` (daemon, `--json`); `app-server` (daemon / `proxy` to control socket / TS+JSON-schema protocol bindings) | yes | 🟢 socket, no ConPTY | 🟢 | **M-API** (strongest) |
| **opencode** | 1.17.8 | `opencode` TUI | ✅ `serve` (HTTP server, `--port/--hostname/--mdns/--cors`) + `attach <url>` (basic-auth `-p/-u`, `--session/--fork`) + `acp` | yes | 🟢 HTTP, no ConPTY | 🟢 | **M-API** (cleanest fit) |
| **claude** | 2.1.170 | default (`claude`); `-p` headless | ❌ no attach-to-live-session server (`--continue` forks a *new* process from history; `mcp serve` gives claude tools, doesn't steer claude) | only path | 🟠 ConPTY interactive proxy = **to prove** | 🟢 | **M-PTY** |
| **agy** | 1.0.10 | `-i/--prompt-interactive`, `--continue` | ❌ no server/attach | only path (headless already PROVEN to need a PTY) | 🔴 **known ConPTY limit** (winpty fallback signposted) | 🟢 | **M-PTY** |

**Known-true vs PoC-gated.** Known: all four installed with interactive modes; codex/opencode structured channels exist; agy-under-PTY proven headless; `portable_pty` in-tree. PoC-gated: whether each structured channel cleanly exposes *observe + inject + interrupt* (codex marks its channel experimental); whether the interactive transparent PTY proxy works on **Windows ConPTY** for claude/agy.

**Consequence for Win+Mac requirement:** codex+opencode deliver FULL live supervision on **both** OSes via M-API with zero ConPTY exposure. claude+agy are full on macOS; on Windows they are the single gated risk, with a winpty fallback and an observe-only floor (§9). So both OSes "really run" supervision from S2 onward; only live-*inject* for the two PTY-only CLIs is Windows-conditional.

---

## 3. Architecture — two planes

### 3.1 Coordination plane (file-based, the transport)
Per repo, under the gitignored `<repo>/.ensemble/` (same root every other module uses — `board.rs:34`, `journal.rs:76`):

- `stream/<member>.ndjson` — append-only **observe** feed; the supervisor tees the session's activity here.
- `control/<member>.ndjson` — append-only **steer** feed; `steer`/`abort` append command records; the supervisor consumes them.

Both reuse the proven append/read discipline of `board.rs` (`FileBoard::post`/`read_since`, `board.rs:55-141`): an `fs2` **exclusive** lock serializes appends, a **shared** lock guards reads, torn trailing lines are repaired and bad lines skipped individually, and `read_since(cursor)` is a lossless positional cursor. This makes multi-process sharing on one machine race-free without inventing new IO discipline.

### 3.2 Enforcement plane (per-member backend, the arm)
A backend grips one live session and bridges it to the coordination plane:

```
trait SessionBackend {
    fn poll_output(&mut self) -> Vec<StreamEvent>; // new observed activity since last poll
    fn inject(&mut self, prompt: &str) -> io::Result<()>;   // push a prompt at the clean point
    fn interrupt(&mut self, hard: bool) -> io::Result<()>;  // soft = Ctrl-C; hard = kill_tree
    fn is_alive(&self) -> bool;
}
```

Two implementations:
- **`PtyProxy`** (claude, agy): owns a `portable_pty` master (pattern from `agy_adapter.rs:39-73`). Tees ANSI-stripped master output (reuse `agy_adapter::strip_ansi`) → `StreamEvent`s; `inject` writes bytes+`\r` to the master writer; `interrupt(false)` writes `0x03`, `interrupt(true)` does a `kill_tree` (pattern from `exec_adapter.rs:94-102`). PLUS a **transparent interactive proxy** so the operator keeps using the TUI: forward operator-stdin→master and master→operator-stdout in raw mode, propagate window resize. This proxy layer is the heavy, OS-divergent part (§11).
- **`ApiChannel`** (codex, opencode): launches/owns the CLI's structured server (`opencode serve` bound `127.0.0.1`+auth; `codex remote-control start`), subscribes to its event protocol → `StreamEvent`s (governance-grade, structured), translates `inject`→send-message and `interrupt`→the server's abort call. The human co-attaches a native client (`opencode attach <url>`). No PTY → no ConPTY.

A single **backend-agnostic supervisor loop** drives any `SessionBackend`: each tick, drain `poll_output()` → append to the `stream` feed; read the `control` feed `read_since(applied_cursor)` → apply each command in order at the clean point → advance cursor; record an `injected`/`interrupted` event back into the stream for audit.

---

## 4. Data flow

**Local (one machine, multi-process):**
```
ensemble steer/abort  ──append──▶  .ensemble/control/<m>.ndjson  ──read_since──▶  supervisor ──apply──▶ live CLI
live CLI ──tee/subscribe──▶ supervisor ──append──▶ .ensemble/stream/<m>.ndjson  ──read_since──▶  ensemble watch
```

**Cross-machine (over the tailnet):** the file-plane is single-machine; cross-machine rides the existing `ensemble serve` agent-host (the same HTTP-over-WireGuard transport `RemoteAdapter` already uses), NOT git (`.ensemble/` is gitignored, never travels via git). New serve routes window onto the node's local feeds (§8):
```
main@A: ensemble watch <m> --node http://100.x.B:7878  ──GET /stream/<m>?since=N──▶  node B reads local stream feed
main@A: ensemble steer <m> --node http://100.x.B:7878  ──POST /control/<m>────────▶  node B appends local control feed
```

---

## 5. File-plane schema

### 5.1 ndjson primitive
New `src/ndjson.rs` exposing a generic feed (board.rs's discipline over arbitrary JSON lines):
```
pub struct Feed { path: PathBuf }
impl Feed {
    pub fn open(path: PathBuf) -> Self;
    pub fn append(&self, line: &str) -> io::Result<usize>;      // exclusive lock; torn-tail repair; returns new cursor
    pub fn read_since(&self, n: usize) -> io::Result<Vec<String>>; // shared lock; skip bad lines; raw JSON lines
}
```
The caller serde-parses each returned line into `StreamEvent` / `ControlCmd`. `board.rs` is the reference implementation; it MAY later be refactored onto `Feed`, but is left untouched now (YAGNI — do not churn proven code).

### 5.2 Stream events (one ndjson object per line, internally tagged `"ev"`)
```
{"ev":"session_start","member":"claude@conductor","cli":"claude","backend":"pty","host":"conductor","pid":12345,"ts":"<rfc3339>"}
{"ev":"turn_start","n":7,"prompt":"<excerpt>","ts":...}      // model began working
{"ev":"output","n":7,"text":"<excerpt chunk>","ts":...}      // observed activity (PTY: stripped tee; API: assistant delta)
{"ev":"tool","n":7,"name":"Edit","detail":"<excerpt>","ts":...}  // API backends only (PTY can't cleanly)
{"ev":"turn_end","n":7,"reply":"<excerpt>","ts":...}
{"ev":"injected","n":8,"from":"main@node-a","prompt":"<excerpt>","ts":...}
{"ev":"interrupted","n":8,"from":"main@node-a","hard":false,"ts":...}
{"ev":"session_end","reason":"exited|killed|eof","ts":...}
```
`n` = monotonic per-session turn counter. `ts` = RFC3339 from `SystemTime`. Text fields excerpted to `board::MAX_BODY` (1500 chars) for hygiene.

### 5.3 Control commands (one ndjson object per line, tagged `"cmd"`)
```
{"cmd":"inject","seq":1,"from":"main@node-a","prompt":"focus on the auth path; skip the UI","ts":...}
{"cmd":"abort","seq":2,"from":"main@node-a","hard":false,"ts":...}
```
`seq` = monotonic per control feed (display/audit). The supervisor tracks an `applied` cursor (= `read_since` index); commands at index ≥ `applied` are applied in order, then the cursor advances — idempotent and lossless via the board cursor guarantee. `prompt` bounded like `MAX_BODY`.

### 5.4 Member-name safety (CRITICAL)
`watch`/`steer`/`abort` and the serve routes all take a `member` from untrusted input (argv / HTTP path). Path helpers confine it to the feed dirs, reusing `journal.rs::sanitize_slug`'s discipline (`journal.rs:56-79`):
```
fn member_stream_path(repo: &Path, member: &str) -> PathBuf  // <repo>/.ensemble/stream/<sanitized>.ndjson
fn member_control_path(repo: &Path, member: &str) -> PathBuf // <repo>/.ensemble/control/<sanitized>.ndjson
```
A hostile member like `../../etc/passwd` must resolve to a direct child of the feed dir with no surviving `..` component — mirror the `journal_path` confinement test (`journal.rs:200-210`). Default member name reuses the already-landed `mcp_install::default_member_name` → `<cli>@<host>`.

---

## 6. Backend interface — per CLI

| CLI | backend | launch | observe | inject | interrupt |
|---|---|---|---|---|---|
| **opencode** | ApiChannel | `opencode serve --hostname 127.0.0.1` (+ auth env); operator `opencode attach <url>` | subscribe server event stream (HTTP/SSE) → structured events | POST message to session | server abort/interrupt call |
| **codex** | ApiChannel | `codex remote-control start` (daemon) / `app-server` control socket | subscribe app-server protocol events | app-server send-message | app-server interrupt |
| **claude** | PtyProxy | `ensemble claude [args]` spawns `claude` under a PTY master | tee master → `strip_ansi` → events | write prompt+`\r` to master | `0x03` to master / `kill_tree` |
| **agy** | PtyProxy | `ensemble agy [args]` spawns `agy -i` under a PTY master | tee master → `strip_ansi` → events | write prompt+`\r` to master | `0x03` to master / `kill_tree` |

**Clean injection point.** Injecting mid-turn is racy (depends on how the CLI buffers input). ApiChannel knows turn state from the protocol → inject waits for idle cleanly. PtyProxy queues `inject` and flushes when the prompt is observed ready if detectable, else applies immediately and records the raciness; `interrupt` (Ctrl-C) is always clean. This is the honest limit flagged in the original analysis.

### 6.1 Operator-facing interface
**Design principle: `ensemble <cli>` enters the CLI's OWN native interface — ensemble never replaces the UI with its own; it co-drives behind the curtain.**

| command | what the operator enters | mechanism behind the curtain |
|---|---|---|
| `ensemble claude` | the real Claude Code TUI, verbatim (+ a one-line ensemble banner) | one PTY master; operator keystrokes AND injected keystrokes share it |
| `ensemble agy` | the real `agy -i` interactive TUI, verbatim | same single-PTY co-input |
| `ensemble opencode` | the native opencode TUI via `opencode attach` to an ensemble-run `opencode serve` | operator TUI + ensemble API driver are two clients of one server |
| `ensemble codex` | native codex TUI **iff** its TUI can attach to the `remote-control` daemon; **else** falls back to PtyProxy wrapping the native codex TUI (re-incurring ConPTY on Windows) | shared server if attach exists, else single PTY |

In every case the operator uses the **native** interface, and an injected prompt appears in the **same live conversation they see** (PtyProxy: injected keystrokes land in the same PTY; ApiChannel: an injected message lands in the same server session). The two input sources (human + ensemble) co-drive one session.

The only place a non-native experience can leak is `ensemble codex` if codex's TUI cannot attach to its daemon — a PoC-gated question (§12 ②).

---

## 7. CLI surface

- `ensemble <cli> [args...]` — wrap-launch supervisor (US-1). Picks PtyProxy (claude/agy) or ApiChannel (codex/opencode); member name = `<cli>@<host>`; writes stream, polls control. Follows the existing `fn <name>_cmd(args: &[String])` dispatch pattern in `main.rs`.
- `ensemble watch <member> [--node <url>] [--since N] [--follow]` — tail the stream feed (US-2 observe). Local = `Feed::read_since`; remote = `GET <node>/stream/<member>?since=N`.
- `ensemble steer <member> "<prompt>" [--node <url>]` — append an `inject` command (US-2 steer). Local = append control feed; remote = `POST <node>/control/<member>`.
- `ensemble abort <member> [--hard] [--node <url>]` — append an `abort` command (`--hard` = kill_tree).

All flag parsing reuses `main.rs`'s `parse_flag`/`has_flag`/positional helpers.

---

## 8. Cross-machine — serve extensions
Extend `serve_loop` (`serve.rs:57-84`), which today routes only `GET /health` and `POST /run`:

- `GET /stream/<member>?since=N` → read the node's local stream `Feed`, return `{ "events": [<json line>...], "cursor": <next> }`.
- `POST /control/<member>` (body = one `ControlCmd`) → append to the node's local control `Feed`, return `{ "cursor": <new> }`.

Bind is unchanged: `resolve_bind` (`serve.rs:28-36`) already prefers the tailnet IPv4, else loopback, never implicit `0.0.0.0`. Member name is re-sanitized server-side (defense in depth). **New security requirement:** the control route is a command channel that can push prompts into a live, possibly permission-skipping CLI — it MUST be gated by a shared secret (`ENSEMBLE_TOKEN` header / `--token`) for cross-machine use; `/run` is out of scope for this change.

---

## 9. Governance & journaling boundary
Live supervision is **observability-grade**, NOT a second governance path:
- The **double-gate landing governance stays exactly where it is** — on the headless conductor path (`conductor.rs`). A PTY-supervised interactive `claude` that edits the repo is NOT auto-double-gated; governed landing still goes through the headless conductor, not the live session. This avoids the governance-blur the original analysis flagged.
- **PTY backends** produce raw, ANSI-stripped, excerpted tee text → advisory only (see drift, steer, interrupt). Not a clean `AgentOutput` transcript.
- **API backends** emit structured turn records and COULD later feed `journal.rs` as governance-grade history — deferred, not required now.

---

## 10. Security & threat model
- **Trust boundary** = the tailnet + same-repo crew (identical to today's `board`/`serve`). WireGuard encrypts the link; serve binds tailnet-only.
- `.ensemble/stream` + `.ensemble/control` are gitignored, local-FS-permissioned (same as `board.jsonl`).
- **Member-name traversal** guarded on every argv/HTTP surface (§5.4).
- **Audit:** every `injected`/`interrupted` event records its `from` origin; inject content is length-bounded.
- **Remote control** endpoints: tailnet-bound, shared-secret gated, never implicit `0.0.0.0`; opencode `serve` bound `127.0.0.1` + basic auth; codex daemon on a local socket.
- **Privilege:** an injected prompt runs with the live session's permissions (including `--dangerously-skip-permissions` if the operator launched that way). A remote steerer is therefore as powerful as the local operator — only trusted tailnet peers. The operator opts into this by wrap-launching; documented.

---

## 11. Windows + macOS acceptance (the hard gate)

| backend | CLIs | macOS arm64 | Windows | acceptance test |
|---|---|---|---|---|
| **ApiChannel** | codex, opencode | 🟢 expected | 🟢 expected (no ConPTY) | start backend → an event appears in `watch` → `steer` injects a prompt the session executes → `abort` interrupts a running turn |
| **PtyProxy** | claude, agy | 🟢 clean Unix PTY | 🟠/🔴 **gated** | same 4 verbs **plus** the operator can transparently use the TUI; Windows: prove on ConPTY, else winpty-class backend |

**Guarantee against "win+mac 都要能真的運行":** S2 lands FULL supervision for codex+opencode on both OSes (M-API, no ConPTY) — the bar is met the day S2 ships. claude+agy: full on macOS; on Windows they are de-risked last with (a) a winpty-class fallback and (b) a worst-case **observe-only floor** — even if interactive ConPTY inject proves intractable, claude/agy on Windows still stream + cooperatively-abort via the headless+file-plane path (S1), so they "really run" supervision, just without live keystroke-inject.

---

## 12. Slice / PoC plan (recommended order; finalized in the implementation plan)

- **S0 — file-plane primitive (lib, cross-platform, no backend).** `ndjson::Feed` (TDD, mirror `board.rs`) + `StreamEvent`/`ControlCmd` types + member-path safety (TDD, mirror `journal` sanitize) + `ensemble watch <member>` local tail. Value: tail any stream; the shared foundation. Double-gate.
- **S1 — control plane + crew wiring (cross-platform, no PTY).** control `Feed` + `ensemble steer`/`abort` + serve `/stream`+`/control` routes (+token) + wire the **headless conductor** to emit stream events at round boundaries and honor `abort` cooperatively at round boundaries. Value: observe + cooperatively-abort the autonomous crew, cross-machine, Win+Mac, **zero ConPTY risk**. Double-gate.
- **S2 — ApiChannel backend (Win+Mac green; first LIVE steer).** `SessionBackend`/supervisor loop + `ApiChannel` + `ensemble opencode` (S2a, cleanest) then `ensemble codex` (S2b). PoC-gated: confirm observe+inject+interrupt over each protocol. Value: the operator's core "live monitor + inject + interrupt other sessions" — delivered cross-platform. Double-gate.
- **S3 — PtyProxy backend (the heavy/risky one, LAST).** `PtyProxy` transparent proxy + `ensemble claude`/`ensemble agy`. **S3a macOS** (clean) → **S3b Windows ConPTY** de-risk (winpty fallback). Sequenced last because S0–S2 already prove the plane + steer + abort semantics, so S3 carries only the NEW PTY-transport risk. Double-gate.

**PoC must-resolve (ordered by risk):** ① claude+agy interactive transparent proxy on **Windows ConPTY** (raw passthrough + resize + inject + Ctrl-C); winpty fallback viability — biggest. ② codex `remote-control`/`app-server` exposes the three verbs stably (experimental) **AND whether codex's interactive TUI can attach to the daemon** — if not, `ensemble codex`'s human side falls back to PtyProxy/ConPTY (§6.1). ③ opencode `serve` event-stream + message + interrupt API shape; co-attach UX. ④ macOS arm64 re-verify all four (expected easiest).

---

## 13. Non-goals (YAGNI)
- Not a general terminal multiplexer / tmux replacement.
- Not recording PTY sessions as governance-grade double-gate evidence (§9 — observability-grade).
- No web UI for `watch` (terminal tail; a UI can ride existing `serve` later).
- No new identity/auth scheme beyond the CLIs' own + the tailnet's shared secret.
- No token-level streaming for PTY backends (raw tee is what the PTY yields).

## 14. Open questions
- PTY clean-injection idle detection: heuristic vs. operator discipline — settle in the S3 PoC.
- Promote API-backend structured turns to `journal` (governance-grade)? Deferred.
- Exact `ENSEMBLE_TOKEN` scheme (header name, env, rotation) — settle in S1's serve extension.
