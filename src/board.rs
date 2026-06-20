//! A persistent, MULTI-PROCESS blackboard backing the `ensemble mcp` crew API. Live CLIs each run
//! their own `ensemble mcp` subprocess (no shared memory), so they coordinate through this shared
//! append-only JSONL file under the repo's `.ensemble/` — `<repo>/.ensemble/board.jsonl`. Reuses
//! `blackboard::Message`. (The conductor's in-memory `Blackboard` stays for a single headless run;
//! this is the cross-process board for live members.)

use crate::blackboard::Message;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Keep a posted body bounded so ONE serialized JSON line stays well under the OS atomic-append size
/// (PIPE_BUF — 4 KiB on Linux). A single bounded `write()` with O_APPEND lands at EOF atomically, so
/// concurrent posts from multiple processes never interleave. Bodies are excerpted to this many chars.
const MAX_BODY: usize = 1500;

/// The shared on-disk board for one repo (session = repo).
pub struct FileBoard {
    path: PathBuf,
}

impl FileBoard {
    /// The board for `repo` lives at `<repo>/.ensemble/board.jsonl`.
    pub fn open(repo: &Path) -> Self {
        Self {
            path: repo.join(".ensemble").join("board.jsonl"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one message (body excerpted to stay atomic-append-safe), creating `.ensemble/` if
    /// needed. Concurrent-safe across processes (O_APPEND of a bounded line is interleave-free).
    pub fn post(&self, from: &str, kind: &str, body: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let msg = Message {
            from: from.to_string(),
            kind: kind.to_string(),
            body: excerpt(body, MAX_BODY),
        };
        let mut line = serde_json::to_string(&msg).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    /// All messages at index ≥ `n` (a malformed/partial line is skipped, never fatal). Empty if the
    /// board file doesn't exist yet.
    pub fn read_since(&self, n: usize) -> std::io::Result<Vec<Message>> {
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let msgs: Vec<Message> = std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect();
        Ok(msgs.into_iter().skip(n).collect())
    }

    /// Total message count — the cursor a poller advances to.
    pub fn len(&self) -> std::io::Result<usize> {
        Ok(self.read_since(0)?.len())
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Excerpt `s` to at most `max` chars (char-boundary safe), appending `…` when truncated.
fn excerpt(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_then_read_roundtrips_with_a_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FileBoard::open(tmp.path());
        assert!(b.is_empty().unwrap());
        b.post("codex", "result", "implemented the parser").unwrap();
        b.post("claude", "verdict", "VERDICT: LGTM").unwrap();
        assert_eq!(b.len().unwrap(), 2);
        let all = b.read_since(0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].from, "codex");
        assert_eq!(all[0].kind, "result");
        assert_eq!(all[0].body, "implemented the parser");
        // a cursor returns only newer messages
        let newer = b.read_since(1).unwrap();
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].from, "claude");
    }

    #[test]
    fn read_missing_board_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FileBoard::open(tmp.path());
        assert!(b.read_since(0).unwrap().is_empty());
        assert_eq!(b.len().unwrap(), 0);
    }

    #[test]
    fn body_is_excerpted_to_stay_atomic_append_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FileBoard::open(tmp.path());
        let huge = "x".repeat(5000);
        b.post("agy", "finding", &huge).unwrap();
        let got = &b.read_since(0).unwrap()[0].body;
        assert!(got.chars().count() <= MAX_BODY + 1, "body must be excerpted, got {}", got.chars().count());
        assert!(got.ends_with('…'), "an excerpted body is marked");
    }

    #[test]
    fn separate_handles_share_the_same_repo_board() {
        // two FileBoard handles on the same repo (≈ two processes) see each other's posts
        let tmp = tempfile::tempdir().unwrap();
        FileBoard::open(tmp.path()).post("a", "question", "anyone on auth?").unwrap();
        let seen = FileBoard::open(tmp.path()).read_since(0).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, "anyone on auth?");
    }
}
