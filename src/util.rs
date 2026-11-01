//! Small shared string utilities — the single home for filename sanitization and char-safe
//! truncation. Before this module these existed as near-identical copies across `worktree.rs`,
//! `repo_sync.rs`, `journal.rs` (sanitize) and `blackboard.rs`, `board.rs`, `test_gate.rs`
//! (excerpt/tail). Keep one owner per behavior here; callers add only their own policy
//! (truncation length, empty-fallback string) on top.

/// Map every char outside `[A-Za-z0-9_-]` to `-`. With `allow_dot`, `.` is also kept — callers
/// that allow dots MUST trim leading/trailing dots themselves so `""`, `"."`, `".."` can never
/// name a traversal/hidden entry. Only the `cli` modules (journal, worktree, repo_sync) call it.
#[cfg_attr(not(feature = "cli"), allow(dead_code))]
pub(crate) fn sanitize_component(s: &str, allow_dot: bool) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || (allow_dot && c == '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// True when `host` looks like a bare IPv4 literal — it contains a `.` and every dot-separated
/// segment is non-empty and all-ASCII-digits (e.g. `127.0.0.1`). Used to decide a host has no
/// meaningful short DNS label, so it should be kept whole rather than reduced to a first label.
pub fn is_ipv4_literal(host: &str) -> bool {
    host.contains('.')
        && host
            .split('.')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
}

/// Keep the FIRST `max` chars (char-boundary safe), appending `…` when truncated.
pub(crate) fn excerpt(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Keep the LAST `max` chars (char-boundary safe), prefixing `…` when truncated. Use for output
/// whose signal is at the end (test failures, tracebacks).
pub(crate) fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let skip = n - max;
        format!("…{}", s.chars().skip(skip).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_maps_unsafe_chars_to_dash() {
        assert_eq!(
            sanitize_component("dispatch/codex-0", false),
            "dispatch-codex-0"
        );
        assert_eq!(sanitize_component("a b.c", false), "a-b-c");
        // dots kept only when allowed
        assert_eq!(sanitize_component("a.b", true), "a.b");
        assert_eq!(sanitize_component("a.b", false), "a-b");
        // underscore and hyphen preserved
        assert_eq!(sanitize_component("keep_-keep", false), "keep_-keep");
    }

    #[test]
    fn is_ipv4_literal_only_for_dotted_numeric_hosts() {
        assert!(is_ipv4_literal("127.0.0.1"));
        assert!(is_ipv4_literal("10.0.0.255"));
        assert!(!is_ipv4_literal("node-b")); // no dot
        assert!(!is_ipv4_literal("node-b.ts.net")); // non-numeric segments
        assert!(!is_ipv4_literal("10..1")); // empty segment
        assert!(!is_ipv4_literal("[::1]")); // v6 literal is not v4
    }

    #[test]
    fn excerpt_keeps_head_and_marks_truncation() {
        assert_eq!(excerpt("short", 10), "short");
        assert_eq!(excerpt("abcdef", 3), "abc…");
        // char-boundary safe across a multi-byte char
        assert_eq!(excerpt("é本x", 2), "é本…");
    }

    #[test]
    fn tail_keeps_end_and_marks_truncation() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("abcdef", 3), "…def");
        assert_eq!(tail("aé本", 2), "…é本");
    }
}
