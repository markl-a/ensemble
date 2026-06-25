//! Live-supervision schema and helpers (design: docs/specs/2026-06-21-live-supervision-design.md). The
//! stream feed (.ensemble/stream/<member>.ndjson) carries `StreamEvent`s a supervisor tees from a live
//! CLI session; `ensemble watch` renders them. Pure (schema + render + path confinement + arg parse);
//! the IO shell lives in main.rs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// One line in a member's stream feed. Internally tagged on `"ev"` (like journal::Entry's `"rec"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum StreamEvent {
    SessionStart {
        member: String,
        cli: String,
        backend: String,
        #[serde(default)]
        host: Option<String>,
        pid: u32,
        ts: String,
    },
    TurnStart {
        n: u64,
        #[serde(default)]
        prompt: String,
        ts: String,
    },
    Output {
        n: u64,
        text: String,
        ts: String,
    },
    Tool {
        n: u64,
        name: String,
        #[serde(default)]
        detail: String,
        ts: String,
    },
    TurnEnd {
        n: u64,
        reply: String,
        ts: String,
    },
    Injected {
        n: u64,
        from: String,
        prompt: String,
        ts: String,
    },
    Interrupted {
        n: u64,
        from: String,
        hard: bool,
        ts: String,
    },
    SessionEnd {
        reason: String,
        ts: String,
    },
}

/// Render one stream event as a single human line for `ensemble watch`.
pub fn render_event(ev: &StreamEvent) -> String {
    match ev {
        StreamEvent::SessionStart {
            member,
            cli,
            backend,
            host,
            pid,
            ..
        } => {
            let h = host.as_deref().unwrap_or("?");
            format!("● session_start  {member} ({cli}/{backend}) host={h} pid={pid}")
        }
        StreamEvent::TurnStart { n, prompt, .. } => {
            format!("▶ turn #{n} start  {}", inline(prompt))
        }
        StreamEvent::Output { n, text, .. } => format!("  #{n} | {}", inline(text)),
        StreamEvent::Tool {
            n, name, detail, ..
        } => format!("  #{n} ⚙ {name}  {}", inline(detail)),
        StreamEvent::TurnEnd { n, reply, .. } => format!("◀ turn #{n} end    {}", inline(reply)),
        StreamEvent::Injected {
            n, from, prompt, ..
        } => {
            format!("⤵ inject #{n} from {from}: {}", inline(prompt))
        }
        StreamEvent::Interrupted { n, from, hard, .. } => {
            format!(
                "✖ interrupt #{n} from {from} ({})",
                if *hard { "hard" } else { "ctrl-c" }
            )
        }
        StreamEvent::SessionEnd { reason, .. } => format!("● session_end  ({reason})"),
    }
}

/// Render one RAW feed line: parse → pretty render; on ANY parse failure (a torn line, or a forward-
/// compat event kind this binary doesn't know) fall back to the raw line so nothing is hidden.
pub fn render_line(raw: &str) -> String {
    // A `StreamEvent` (tagged on "ev") wins — it's the more specific member-session shape. A KNOWN kind
    // renders pretty.
    if let Ok(ev) = serde_json::from_str::<StreamEvent>(raw) {
        return render_event(&ev);
    }
    // An "ev"-tagged line that did NOT parse is an unknown/future event kind (or a torn one): show it RAW.
    // It must NEVER fall through to `Message` parsing — a future event that coincidentally carries
    // from/kind/body would otherwise be mis-rendered as a governed-run post (serde ignores the extra
    // "ev" field). Forward-compat: unknown event kinds are shown, never hidden or mislabeled.
    let is_ev_tagged = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .is_some_and(|v| v.get("ev").is_some());
    if !is_ev_tagged {
        // No "ev" tag → it may be a governed-run blackboard `Message` ({from,kind,body}), rendered so one
        // viewer tails BOTH member sessions and live `ensemble run`s (S1a).
        if let Ok(m) = serde_json::from_str::<crate::blackboard::Message>(raw) {
            return format!("  [{} · {}] {}", m.from, m.kind, inline(&m.body));
        }
    }
    format!("? {}", raw.trim())
}

/// Collapse a possibly-multiline excerpt to one whitespace-normalized line, bounded for the watch view.
fn inline(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 120 {
        one
    } else {
        format!("{}…", one.chars().take(120).collect::<String>())
    }
}

/// The stream feed path for `member` under `repo`, confined to `<repo>/.ensemble/stream/`. `member`
/// comes from untrusted input (argv / HTTP path), so it is reduced to ONE safe filename component by
/// journal's shared path-component sanitizer.
pub fn member_stream_path(repo: &Path, member: &str) -> PathBuf {
    repo.join(".ensemble")
        .join("stream")
        .join(format!("{}.ndjson", crate::journal::sanitize_slug(member)))
}

/// A live sink the conductor mirrors each blackboard post into, so a governed `ensemble run` is
/// watchable in real time (S1a). Best-effort by contract: an implementation must never let a write
/// failure surface — it cannot be allowed to change a run's outcome (mirrors journal's discipline).
pub trait RunObserver: Send + Sync {
    fn post(&self, m: &crate::blackboard::Message);
}

/// The production `RunObserver`: mirrors each blackboard post into an append-only `ndjson::Feed` (one
/// `Message` JSON per line) for `ensemble watch <name> --follow` to tail. Best-effort — a serialize or
/// write failure is swallowed so live supervision can NEVER change a governed run's outcome.
pub struct FeedObserver {
    feed: crate::ndjson::Feed,
}

impl FeedObserver {
    pub fn new(feed: crate::ndjson::Feed) -> Self {
        Self { feed }
    }
}

impl RunObserver for FeedObserver {
    fn post(&self, m: &crate::blackboard::Message) {
        if let Ok(line) = serde_json::to_string(m) {
            let _ = self.feed.append(&line);
        }
    }
}

/// One line in a run's CONTROL feed (.ensemble/control/<name>.ndjson) — the operator's channel to steer
/// or interrupt a live governed run (S1b). Internally tagged on `"cmd"` (like StreamEvent's `"ev"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ControlCmd {
    /// Inject `prompt` into the NEXT round's implementer prompt (redirect a run that is drifting).
    Steer { from: String, prompt: String },
    /// Stop the run: cleanly at the next round boundary, or `hard` = kill the running CLI immediately.
    Abort {
        from: String,
        #[serde(default)]
        hard: bool,
    },
}

/// The control feed path for `name` under `repo`, confined to `<repo>/.ensemble/control/` by the same
/// shared one-component sanitizer as the stream feed (untrusted argv / HTTP path).
pub fn member_control_path(repo: &Path, name: &str) -> PathBuf {
    repo.join(".ensemble")
        .join("control")
        .join(format!("{}.ndjson", crate::journal::sanitize_slug(name)))
}

/// Shared signals a control watcher feeds (from the control feed) and the conductor reads at round
/// boundaries: an abort request (`hard` ⇒ kill the running CLI now), plus a queue of steer prompts to
/// inject into the next round. The abort flag is the SAME `Arc<AtomicBool>` the conductor checks, so a
/// control-feed abort and a Ctrl-C converge.
#[derive(Default)]
pub struct ControlState {
    abort: Arc<AtomicBool>,
    hard: Arc<AtomicBool>,
    steers: Mutex<Vec<String>>,
}

impl ControlState {
    /// The abort flag to share with the conductor (so a control abort == the conductor's own abort).
    pub fn abort_flag(&self) -> Arc<AtomicBool> {
        self.abort.clone()
    }
    /// The HARD-abort flag to hand to a running adapter: when set (an `abort --hard`), the adapter kills
    /// its child mid-turn instead of waiting for the round boundary. A clean abort never sets it.
    pub fn hard_flag(&self) -> Arc<AtomicBool> {
        self.hard.clone()
    }
    pub fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }
    pub fn hard(&self) -> bool {
        self.hard.load(Ordering::Relaxed)
    }
    /// Drain and return the queued steer prompts (each consumed once, injected into the next round).
    pub fn take_steers(&self) -> Vec<String> {
        std::mem::take(&mut self.steers.lock().unwrap())
    }
    /// Test/seed helper: queue a steer without going through the feed.
    pub fn push_steer(&self, prompt: &str) {
        self.steers.lock().unwrap().push(prompt.to_string());
    }
}

/// Apply every NEW control line at index >= `cursor` to `st`, advancing `cursor` past what was read.
/// Unknown/torn lines are skipped (forward-compat). Best-effort: a read error leaves state untouched
/// (a flaky control feed can never crash a run).
pub fn drain_control(feed: &crate::ndjson::Feed, cursor: &mut usize, st: &ControlState) {
    let lines = match feed.read_since(*cursor) {
        Ok(l) => l,
        Err(_) => return,
    };
    for line in &lines {
        if let Ok(cmd) = serde_json::from_str::<ControlCmd>(line) {
            match cmd {
                ControlCmd::Steer { prompt, .. } => st.steers.lock().unwrap().push(prompt),
                ControlCmd::Abort { hard, .. } => {
                    if hard {
                        st.hard.store(true, Ordering::Relaxed);
                    }
                    st.abort.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    *cursor += lines.len();
}

/// Parsed `ensemble watch` arguments (pure; the IO shell in main.rs consumes this).
#[derive(Debug, PartialEq)]
pub struct WatchArgs {
    pub member: Option<String>,
    pub repo: Option<String>,
    pub node: Option<String>,
    pub team: Option<String>,
    pub json: bool,
    pub since: usize,
    pub follow: bool,
}

/// Parse the argv of `ensemble watch <member> [--repo <p>] [--since <n>] [--follow]`. `args` is the full
/// process argv (args[0]=exe, args[1]="watch"). The first non-flag token is the member; unknown
/// `--flags` are skipped (no value assumed); a non-numeric `--since` falls back to 0.
pub fn parse_watch_args(args: &[String]) -> WatchArgs {
    let mut out = WatchArgs {
        member: None,
        repo: None,
        node: None,
        team: None,
        json: false,
        since: 0,
        follow: false,
    };
    let mut i = 2; // skip exe + "watch"
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => {
                out.repo = args.get(i + 1).cloned();
                i += 2;
            }
            "--node" => {
                out.node = args.get(i + 1).cloned();
                i += 2;
            }
            "--team" => {
                out.team = args.get(i + 1).cloned();
                i += 2;
            }
            "--token" => {
                i += 2;
            }
            "--since" => {
                out.since = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
                i += 2;
            }
            "--json" => {
                out.json = true;
                i += 1;
            }
            "--follow" => {
                out.follow = true;
                i += 1;
            }
            a if a.starts_with("--") => i += 1,
            _ => {
                if out.member.is_none() {
                    out.member = Some(args[i].clone());
                }
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_event_roundtrips_with_the_ev_tag() {
        let ev = StreamEvent::TurnStart {
            n: 7,
            prompt: "do the auth path".into(),
            ts: "T".into(),
        };
        let line = serde_json::to_string(&ev).unwrap();
        assert!(line.contains(r#""ev":"turn_start""#), "got {line}");
        assert!(line.contains(r#""n":7"#));
        let back: StreamEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn injected_event_roundtrips() {
        let ev = StreamEvent::Injected {
            n: 8,
            from: "main@yoyogood".into(),
            prompt: "focus".into(),
            ts: "T".into(),
        };
        let back: StreamEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn render_line_pretty_prints_a_known_event() {
        let raw = r#"{"ev":"injected","n":8,"from":"main@z13","prompt":"skip the UI","ts":"T"}"#;
        let s = render_line(raw);
        assert!(s.contains("inject #8"), "got {s}");
        assert!(
            s.contains("main@z13") && s.contains("skip the UI"),
            "got {s}"
        );
    }

    #[test]
    fn render_line_falls_back_on_unknown_or_torn_lines() {
        // a forward-compat event kind this binary doesn't know must NOT be hidden
        let future = r#"{"ev":"some_future_kind","x":1}"#;
        assert!(
            render_line(future).contains("some_future_kind"),
            "unknown kind shown raw"
        );
        // a valid-JSON but non-event line is shown raw, not dropped
        assert!(render_line("{}").starts_with('?'));
    }

    #[test]
    fn render_line_pretty_prints_a_blackboard_message() {
        // a governed-run blackboard post (no "ev" tag) renders as "[from · kind] body", not raw — so
        // `ensemble watch` can tail a live `ensemble run` (S1a), not only member-session StreamEvents.
        let raw = r#"{"from":"codex","kind":"result","body":"implemented the parser"}"#;
        let s = render_line(raw);
        assert!(s.contains("codex") && s.contains("result"), "got {s}");
        assert!(s.contains("implemented the parser"), "got {s}");
        assert!(
            !s.starts_with('?'),
            "a valid Message must not fall back to raw: {s}"
        );
        // a StreamEvent still wins (more specific, tagged on "ev")
        assert!(
            render_line(r#"{"ev":"turn_start","n":1,"prompt":"do it","ts":"T"}"#)
                .contains("turn #1")
        );
        // genuine garbage still falls back to raw
        assert!(render_line("not json").starts_with('?'));
        // forward-compat: an UNKNOWN future event kind is shown RAW even if it coincidentally carries
        // from/kind/body — an "ev"-tagged line must NEVER be mis-parsed as a blackboard Message.
        let ev_future = r#"{"ev":"future_kind","from":"x","kind":"y","body":"z"}"#;
        assert!(
            render_line(ev_future).starts_with('?'),
            "ev-tagged unknown must be raw: {}",
            render_line(ev_future)
        );
    }

    #[test]
    fn feed_observer_appends_a_parseable_message_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run.ndjson");
        let obs = FeedObserver::new(crate::ndjson::Feed::open(path.clone()));
        obs.post(&crate::blackboard::Message {
            from: "codex".into(),
            kind: "result".into(),
            body: "did the thing".into(),
        });
        let lines = crate::ndjson::Feed::open(path).read_since(0).unwrap();
        assert_eq!(lines.len(), 1, "one post → one feed line");
        let rendered = render_line(&lines[0]);
        assert!(
            rendered.contains("codex") && rendered.contains("did the thing"),
            "rendered: {rendered}"
        );
    }

    #[test]
    fn control_cmd_roundtrips_tagged_on_cmd() {
        let s = ControlCmd::Steer {
            from: "main@z13".into(),
            prompt: "skip the UI".into(),
        };
        let line = serde_json::to_string(&s).unwrap();
        assert!(line.contains(r#""cmd":"steer""#), "got {line}");
        assert_eq!(serde_json::from_str::<ControlCmd>(&line).unwrap(), s);
        let a = ControlCmd::Abort {
            from: "main@z13".into(),
            hard: true,
        };
        let aline = serde_json::to_string(&a).unwrap();
        assert!(
            aline.contains(r#""cmd":"abort""#) && aline.contains(r#""hard":true"#),
            "got {aline}"
        );
        assert_eq!(serde_json::from_str::<ControlCmd>(&aline).unwrap(), a);
        // `hard` defaults to false when omitted (a clean abort line stays minimal)
        let clean: ControlCmd = serde_json::from_str(r#"{"cmd":"abort","from":"m"}"#).unwrap();
        assert_eq!(
            clean,
            ControlCmd::Abort {
                from: "m".into(),
                hard: false
            }
        );
    }

    #[test]
    fn member_control_path_confines_a_hostile_name() {
        use std::path::Path;
        let repo = Path::new("/tmp/repo");
        let control = repo.join(".ensemble").join("control");
        let p = member_control_path(repo, "../../etc/passwd");
        assert_eq!(
            p.parent().unwrap(),
            control,
            "must be a direct child of control/"
        );
        assert!(
            !p.components().any(|c| c.as_os_str() == ".."),
            "no traversal survives: {p:?}"
        );
    }

    #[test]
    fn drain_control_applies_steer_and_abort() {
        let tmp = tempfile::tempdir().unwrap();
        let feed = crate::ndjson::Feed::open(tmp.path().join("c.ndjson"));
        feed.append(
            &serde_json::to_string(&ControlCmd::Steer {
                from: "m".into(),
                prompt: "focus".into(),
            })
            .unwrap(),
        )
        .unwrap();
        feed.append(
            &serde_json::to_string(&ControlCmd::Abort {
                from: "m".into(),
                hard: true,
            })
            .unwrap(),
        )
        .unwrap();
        let st = ControlState::default();
        let mut cursor = 0usize;
        drain_control(&feed, &mut cursor, &st);
        assert_eq!(cursor, 2, "cursor advanced past both lines");
        assert_eq!(st.take_steers(), vec!["focus".to_string()]);
        assert!(st.aborted() && st.hard(), "hard abort sets both flags");
        // draining again with no new lines is a no-op (cursor already at end)
        drain_control(&feed, &mut cursor, &st);
        assert_eq!(cursor, 2);
        assert!(st.take_steers().is_empty(), "steers consumed once");
    }

    #[test]
    fn member_stream_path_confines_a_hostile_member() {
        use std::path::Path;
        let repo = Path::new("/tmp/repo");
        let stream = repo.join(".ensemble").join("stream");
        let p = member_stream_path(repo, "../../etc/passwd");
        assert!(
            p.starts_with(&stream),
            "member must not escape the stream dir: {p:?}"
        );
        assert_eq!(
            p.parent().unwrap(),
            stream,
            "must be a direct child of stream/"
        );
        assert!(
            !p.components().any(|c| c.as_os_str() == ".."),
            "no traversal survives: {p:?}"
        );
    }

    #[test]
    fn member_stream_path_sanitizes_the_member_into_one_component() {
        use std::path::Path;
        // reuses journal's sanitizer: '@' in the canonical <cli>@<host> name becomes '-' on disk; the
        // supervisor and `watch` compute the SAME path, so the logical member id still resolves.
        let p = member_stream_path(Path::new("/r"), "claude@z13");
        assert!(p.ends_with("claude-z13.ndjson"), "got {p:?}");
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_watch_args_basic() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "claude@z13"]));
        assert_eq!(w.member.as_deref(), Some("claude@z13"));
        assert_eq!(w.since, 0);
        assert!(!w.follow);
        assert_eq!(w.repo, None);
        assert_eq!(w.node, None);
    }

    #[test]
    fn parse_watch_args_all_flags() {
        let w = parse_watch_args(&argv(&[
            "ensemble",
            "watch",
            "--since",
            "5",
            "claude@z13",
            "--follow",
            "--repo",
            "/r",
            "--node",
            "macbook",
            "--team",
            "ops",
        ]));
        assert_eq!(w.member.as_deref(), Some("claude@z13"));
        assert_eq!(w.since, 5);
        assert!(w.follow);
        assert_eq!(w.repo.as_deref(), Some("/r"));
        assert_eq!(w.node.as_deref(), Some("macbook"));
        assert_eq!(w.team.as_deref(), Some("ops"));
    }

    #[test]
    fn parse_watch_args_json_mode() {
        let w = parse_watch_args(&argv(&[
            "ensemble",
            "watch",
            "cli@node",
            "--json",
            "--team",
            "team-main",
            "--since",
            "7",
        ]));
        assert_eq!(w.member.as_deref(), Some("cli@node"));
        assert!(w.json);
        assert_eq!(w.team.as_deref(), Some("team-main"));
        assert_eq!(w.since, 7);
    }

    #[test]
    fn parse_watch_args_node_value_is_not_a_member() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "--node", "macbook"]));
        assert_eq!(w.member, None);
        assert_eq!(w.node.as_deref(), Some("macbook"));
    }

    #[test]
    fn parse_watch_args_token_value_is_not_a_member() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "--token", "secret", "run-1"]));
        assert_eq!(w.member.as_deref(), Some("run-1"));
    }

    #[test]
    fn parse_watch_args_since_nonnumber_falls_back_to_zero() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "m", "--since", "abc"]));
        assert_eq!(w.since, 0);
    }

    #[test]
    fn parse_watch_args_missing_member_is_none() {
        let w = parse_watch_args(&argv(&["ensemble", "watch", "--follow"]));
        assert_eq!(w.member, None);
    }
}
