//! Per-run journal (design §Phase-1 step 2). After a worktree run, the conductor appends the
//! blackboard transcript (implementer result, each round's test result, reviewer verdicts, findings)
//! plus exactly one terminal `decision` to `.ensemble/runs/<slug>.jsonl` — a replayable record so
//! the operator can SEE the collaboration. One JSON object per line (JSONL); the message entries
//! reuse `blackboard::Message` verbatim.

use crate::blackboard::Message;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One line in a run's journal. The transcript is a sequence of `Msg` entries followed by exactly
/// one terminal `Decision` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rec", rename_all = "snake_case")]
pub enum Entry {
    /// A blackboard message, reused verbatim.
    Msg(Message),
    /// The run's terminal outcome (last line). `outcome` is "landed" or "escalated"; `detail` is the
    /// kept branch (landed) or the escalation reason (escalated).
    Decision {
        outcome: String,
        detail: String,
        rounds: u32,
    },
}

/// Render a run as JSONL: one line per message, then one terminal decision line. Pure (no I/O).
pub fn render(msgs: &[Message], outcome: &str, detail: &str, rounds: u32) -> String {
    let mut s = String::new();
    for m in msgs {
        // serde_json::to_string on a derived enum can't fail; if it ever did we'd rather drop one
        // line than panic a run, so skip on the impossible error.
        if let Ok(line) = serde_json::to_string(&Entry::Msg(m.clone())) {
            s.push_str(&line);
            s.push('\n');
        }
    }
    let decision = Entry::Decision {
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        rounds,
    };
    if let Ok(line) = serde_json::to_string(&decision) {
        s.push_str(&line);
        s.push('\n');
    }
    s
}

/// The journal path for a run slug: `<repo>/.ensemble/runs/<slug>.jsonl`.
pub fn journal_path(repo: &Path, slug: &str) -> PathBuf {
    repo.join(".ensemble")
        .join("runs")
        .join(format!("{slug}.jsonl"))
}

/// Write a run's JSONL to its journal path, creating `.ensemble/runs/` if needed. Returns the path.
pub fn write_run(repo: &Path, slug: &str, jsonl: &str) -> io::Result<PathBuf> {
    let path = journal_path(repo, slug);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, jsonl)?;
    Ok(path)
}

/// Parse a journal back into entries (blank lines skipped) — for reading a run's transcript.
pub fn parse(jsonl: &str) -> Result<Vec<Entry>, serde_json::Error> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(from: &str, kind: &str, body: &str) -> Message {
        Message {
            from: from.into(),
            kind: kind.into(),
            body: body.into(),
        }
    }

    #[test]
    fn render_emits_one_msg_line_each_then_a_terminal_decision() {
        let msgs = vec![
            msg("codex", "result", "implemented the parser"),
            msg("test", "test_pass", "ok"),
            msg("claude", "verdict", "VERDICT: LGTM"),
        ];
        let jsonl = render(&msgs, "landed", "ensemble/x-0", 1);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 4, "3 messages + 1 decision");
        let entries = parse(&jsonl).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0], Entry::Msg(msgs[0].clone()));
        assert_eq!(entries[2], Entry::Msg(msgs[2].clone()));
        match &entries[3] {
            Entry::Decision {
                outcome,
                detail,
                rounds,
            } => {
                assert_eq!(outcome, "landed");
                assert_eq!(detail, "ensemble/x-0");
                assert_eq!(*rounds, 1);
            }
            other => panic!("last entry must be the decision: {other:?}"),
        }
    }

    #[test]
    fn render_with_no_messages_is_just_the_decision_line() {
        let jsonl = render(&[], "escalated", "max rounds reached", 3);
        assert_eq!(jsonl.lines().count(), 1);
        let entries = parse(&jsonl).unwrap();
        assert!(matches!(entries.as_slice(), [Entry::Decision { .. }]));
    }

    #[test]
    fn write_run_creates_runs_dir_and_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let jsonl = render(&[msg("codex", "result", "did it")], "landed", "ensemble/s-0", 1);
        let path = write_run(repo, "my-slug-7", &jsonl).unwrap();
        assert_eq!(path, journal_path(repo, "my-slug-7"));
        assert!(path.exists(), "journal file must exist after write_run");
        let back = fs::read_to_string(&path).unwrap();
        assert_eq!(back, jsonl, "written bytes must match the rendered JSONL");
    }

    #[test]
    fn msg_entry_serializes_with_the_rec_tag_and_message_fields_flat() {
        let line = serde_json::to_string(&Entry::Msg(msg("codex", "result", "hi"))).unwrap();
        // internally-tagged: the discriminant + Message's fields share one object
        assert!(line.contains("\"rec\":\"msg\""), "got {line}");
        assert!(line.contains("\"from\":\"codex\""), "got {line}");
        assert!(line.contains("\"kind\":\"result\""), "got {line}");
    }
}
