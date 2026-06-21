//! Live-supervision schema and helpers (design: docs/specs/2026-06-21-live-supervision-design.md). The
//! stream feed (.ensemble/stream/<member>.ndjson) carries `StreamEvent`s a supervisor tees from a live
//! CLI session; `ensemble watch` renders them. Pure (schema + render + path confinement + arg parse);
//! the IO shell lives in main.rs.

use serde::{Deserialize, Serialize};

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
        StreamEvent::SessionStart { member, cli, backend, host, pid, .. } => {
            let h = host.as_deref().unwrap_or("?");
            format!("● session_start  {member} ({cli}/{backend}) host={h} pid={pid}")
        }
        StreamEvent::TurnStart { n, prompt, .. } => format!("▶ turn #{n} start  {}", inline(prompt)),
        StreamEvent::Output { n, text, .. } => format!("  #{n} | {}", inline(text)),
        StreamEvent::Tool { n, name, detail, .. } => format!("  #{n} ⚙ {name}  {}", inline(detail)),
        StreamEvent::TurnEnd { n, reply, .. } => format!("◀ turn #{n} end    {}", inline(reply)),
        StreamEvent::Injected { n, from, prompt, .. } => {
            format!("⤵ inject #{n} from {from}: {}", inline(prompt))
        }
        StreamEvent::Interrupted { n, from, hard, .. } => {
            format!("✖ interrupt #{n} from {from} ({})", if *hard { "hard" } else { "ctrl-c" })
        }
        StreamEvent::SessionEnd { reason, .. } => format!("● session_end  ({reason})"),
    }
}

/// Render one RAW feed line: parse → pretty render; on ANY parse failure (a torn line, or a forward-
/// compat event kind this binary doesn't know) fall back to the raw line so nothing is hidden.
pub fn render_line(raw: &str) -> String {
    match serde_json::from_str::<StreamEvent>(raw) {
        Ok(ev) => render_event(&ev),
        Err(_) => format!("? {}", raw.trim()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_event_roundtrips_with_the_ev_tag() {
        let ev = StreamEvent::TurnStart { n: 7, prompt: "do the auth path".into(), ts: "T".into() };
        let line = serde_json::to_string(&ev).unwrap();
        assert!(line.contains(r#""ev":"turn_start""#), "got {line}");
        assert!(line.contains(r#""n":7"#));
        let back: StreamEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn injected_event_roundtrips() {
        let ev = StreamEvent::Injected {
            n: 8, from: "main@yoyogood".into(), prompt: "focus".into(), ts: "T".into(),
        };
        let back: StreamEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn render_line_pretty_prints_a_known_event() {
        let raw = r#"{"ev":"injected","n":8,"from":"main@z13","prompt":"skip the UI","ts":"T"}"#;
        let s = render_line(raw);
        assert!(s.contains("inject #8"), "got {s}");
        assert!(s.contains("main@z13") && s.contains("skip the UI"), "got {s}");
    }

    #[test]
    fn render_line_falls_back_on_unknown_or_torn_lines() {
        // a forward-compat event kind this binary doesn't know must NOT be hidden
        let future = r#"{"ev":"some_future_kind","x":1}"#;
        assert!(render_line(future).contains("some_future_kind"), "unknown kind shown raw");
        // a valid-JSON but non-event line is shown raw, not dropped
        assert!(render_line("{}").starts_with('?'));
    }
}
