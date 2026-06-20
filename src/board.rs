//! A persistent, MULTI-PROCESS blackboard backing the `ensemble mcp` crew API. Live CLIs each run
//! their own `ensemble mcp` subprocess (no shared memory), so they coordinate through a shared
//! append-only JSONL file under the repo's `.ensemble/` — `<repo>/.ensemble/board.jsonl`. Reuses
//! `blackboard::Message`. (The conductor's in-memory `Blackboard` stays for a single headless run;
//! this is the cross-process board for live members.)
//!
//! Posts are SERIALIZED by an OS advisory file lock (`fs2`), so the append order == the file order
//! == the read order: a positional `read_since(n)` cursor is therefore LOSSLESS even under concurrent
//! writers (no out-of-order publish). The lock is kernel-managed and auto-released when a holder dies,
//! so a crash can't wedge the board; a crash mid-append leaves at most one torn trailing line, which
//! readers skip (and the next post re-appends cleanly) — never corrupting earlier messages. Readers
//! take a SHARED lock so they never observe a half-written line.

use crate::blackboard::Message;
use fs2::FileExt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Bound a posted body so the board stays readable (hygiene). Excerpted to this many chars.
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

    /// Append one message under an EXCLUSIVE lock (so concurrent posts from any process serialize),
    /// creating `.ensemble/` if needed. The lock makes the append totally ordered → the cursor is
    /// lossless.
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
            .read(true)
            .append(true)
            .open(&self.path)?;
        f.lock_exclusive()?; // blocks until no other writer holds it; released on unlock/close
        let r = append_terminated(&mut f, line.as_bytes());
        let _ = f.unlock();
        r
    }

    /// All messages at index ≥ `n`, in post order, taken under a SHARED lock (so a concurrent append
    /// can't expose a half-written line). Read as BYTES split on `\n` and parsed per line, so a
    /// malformed OR non-UTF-8 line is skipped INDIVIDUALLY — it never stops the scan or hides later
    /// messages. Empty if the board doesn't exist yet.
    pub fn read_since(&self, n: usize) -> std::io::Result<Vec<Message>> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        f.lock_shared()?;
        let mut bytes = Vec::new();
        let read = f.read_to_end(&mut bytes);
        let _ = f.unlock();
        read?;
        let msgs: Vec<Message> = bytes
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_slice::<Message>(l).ok())
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

/// Append `line` to `f` (held under an exclusive lock, opened read+append). REPAIRS a torn tail
/// first: if the file is non-empty and does NOT end in `\n` (a previous writer crashed mid-append, or
/// a partial write), terminate that partial line with a `\n` before appending — otherwise O_APPEND
/// would weld our new message onto the partial bytes, making ONE malformed line that a reader skips,
/// swallowing this post. The partial line stays its own (skipped) line; our message lands clean.
fn append_terminated(f: &mut std::fs::File, line: &[u8]) -> std::io::Result<()> {
    let len = f.metadata()?.len();
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?; // O_APPEND → lands at EOF, closing the torn line
        }
    }
    f.write_all(line)
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
        assert_eq!(all[1].from, "claude"); // append order preserved
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
    fn body_is_excerpted_for_hygiene() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FileBoard::open(tmp.path());
        let huge = "x".repeat(5000);
        b.post("agy", "finding", &huge).unwrap();
        let got = &b.read_since(0).unwrap()[0].body;
        assert!(got.chars().count() <= MAX_BODY + 1, "body excerpted, got {}", got.chars().count());
        assert!(got.ends_with('…'));
    }

    #[test]
    fn separate_handles_share_the_same_repo_board() {
        let tmp = tempfile::tempdir().unwrap();
        FileBoard::open(tmp.path()).post("a", "question", "anyone on auth?").unwrap();
        let seen = FileBoard::open(tmp.path()).read_since(0).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, "anyone on auth?");
    }

    #[test]
    fn torn_tail_is_repaired_so_the_next_post_is_not_swallowed() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FileBoard::open(tmp.path());
        b.post("a", "result", "first").unwrap();
        // simulate a crash mid-append: a partial line with NO trailing newline
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(b.path()).unwrap();
            f.write_all(b"{\"from\":\"x\",\"kind\":\"resul").unwrap();
        }
        assert_eq!(b.read_since(0).unwrap().len(), 1, "the torn partial line is skipped");
        // the NEXT post must not be welded onto the partial bytes and lost
        b.post("c", "verdict", "third").unwrap();
        let all = b.read_since(0).unwrap();
        let bodies: Vec<&str> = all.iter().map(|m| m.body.as_str()).collect();
        assert!(bodies.contains(&"first") && bodies.contains(&"third"), "got {bodies:?}");
        assert_eq!(all.len(), 2, "torn line skipped; both real messages kept");
    }

    #[test]
    fn a_bad_line_is_skipped_without_hiding_later_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let b = FileBoard::open(tmp.path());
        b.post("a", "result", "one").unwrap();
        // a non-UTF-8 / malformed line in the MIDDLE (properly newline-terminated)
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(b.path()).unwrap();
            f.write_all(&[0xff, 0xfe, b'\n']).unwrap();
        }
        b.post("c", "result", "three").unwrap();
        let bodies: Vec<String> = b.read_since(0).unwrap().iter().map(|m| m.body.clone()).collect();
        assert!(
            bodies.iter().any(|s| s == "one") && bodies.iter().any(|s| s == "three"),
            "a bad middle line must not stop the scan: {bodies:?}"
        );
    }

    #[test]
    fn concurrent_posts_serialize_with_no_loss_or_corruption() {
        // many threads (each its own File/lock) posting at once: the exclusive lock serializes the
        // appends, so all land and each line is individually parseable (no interleave).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::thread::scope(|s| {
            for i in 0..20 {
                let dir = dir.clone();
                s.spawn(move || {
                    FileBoard::open(&dir)
                        .post("w", "result", &format!("msg-{i}"))
                        .unwrap();
                });
            }
        });
        let all = FileBoard::open(&dir).read_since(0).unwrap();
        assert_eq!(all.len(), 20, "every concurrent post survives");
        let mut bodies: Vec<String> = all.iter().map(|m| m.body.clone()).collect();
        bodies.sort();
        let mut want: Vec<String> = (0..20).map(|i| format!("msg-{i}")).collect();
        want.sort();
        assert_eq!(bodies, want, "no message lost or corrupted");
    }
}
