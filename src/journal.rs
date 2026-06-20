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

/// Reduce a slug to ONE safe filename component inside `.ensemble/runs`. `write_run`/`journal_path`
/// are public, so a caller-supplied slug must never escape the runs dir via path separators or `..`.
/// (The internal caller `Worktree::slug()` is already sanitized; this guards the public surface.)
/// Note: this does NOT truncate — truncating could chop the seq suffix and reintroduce collisions.
fn sanitize_slug(slug: &str) -> String {
    let cleaned: String = slug
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() => c,
            '-' | '_' | '.' => c,
            _ => '-',
        })
        .collect();
    // strip leading/trailing dots so "", ".", ".." can never name a traversal/hidden entry
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "run".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The canonical journal path for a run slug: `<repo>/.ensemble/runs/<safe-slug>.jsonl`.
pub fn journal_path(repo: &Path, slug: &str) -> PathBuf {
    repo.join(".ensemble")
        .join("runs")
        .join(format!("{}.jsonl", sanitize_slug(slug)))
}

/// Write a run's JSONL under `.ensemble/runs/` and return the path it landed at. NEVER truncates an
/// existing journal: the worktree seq counter is process-local, so a later process can reuse a slug
/// once the prior run's branch is gone — so we create-new and disambiguate to `<slug>.1.jsonl`,
/// `<slug>.2.jsonl`, … keeping every run's record intact.
pub fn write_run(repo: &Path, slug: &str, jsonl: &str) -> io::Result<PathBuf> {
    use std::io::Write;
    let dir = repo.join(".ensemble").join("runs");
    fs::create_dir_all(&dir)?;
    let stem = sanitize_slug(slug);
    for n in 0u32..=u32::MAX {
        let path = if n == 0 {
            dir.join(format!("{stem}.jsonl")) // == journal_path(repo, slug)
        } else {
            dir.join(format!("{stem}.{n}.jsonl"))
        };
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(jsonl.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "exhausted journal filename disambiguation",
    ))
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
    fn write_run_never_overwrites_an_existing_run() {
        // a later process can reuse a slug once the prior branch is gone — the second run's journal
        // must NOT clobber the first (the seq counter is process-local, so slugs aren't globally unique).
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let a = write_run(repo, "dup", "AAA\n").unwrap();
        let b = write_run(repo, "dup", "BBB\n").unwrap();
        assert_ne!(a, b, "a second same-slug run must land in its own file");
        assert_eq!(fs::read_to_string(&a).unwrap(), "AAA\n", "first journal preserved");
        assert_eq!(fs::read_to_string(&b).unwrap(), "BBB\n");
        let n = fs::read_dir(repo.join(".ensemble/runs")).unwrap().count();
        assert_eq!(n, 2, "both runs kept");
    }

    #[test]
    fn journal_path_confines_a_hostile_slug_to_the_runs_dir() {
        let repo = Path::new("/tmp/repo");
        let runs = repo.join(".ensemble").join("runs");
        let p = journal_path(repo, "../../etc/passwd");
        assert!(p.starts_with(&runs), "slug must not escape the runs dir: {p:?}");
        assert_eq!(p.parent().unwrap(), runs, "must be a direct child of runs");
        assert!(
            !p.components().any(|c| c.as_os_str() == ".."),
            "no traversal component may survive: {p:?}"
        );
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
