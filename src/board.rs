//! A persistent, MULTI-PROCESS blackboard backing the `ensemble mcp` crew API. Live CLIs each run
//! their own `ensemble mcp` subprocess (no shared memory), so they coordinate through shared state
//! under the repo's `.ensemble/`. Reuses `blackboard::Message`. (The conductor's in-memory
//! `Blackboard` stays for a single headless run; this is the cross-process board for live members.)
//!
//! Storage = a DIRECTORY of one-file-per-message (`<repo>/.ensemble/board/<ts>-<pid>-<seq>.json`),
//! NOT a shared append-only file. Each post is written to a temp file then ATOMICALLY renamed into
//! place, so concurrent posts from different processes never contend, never interleave/corrupt, and
//! a writer that crashes mid-post leaves only an ignored `.tmp` — never a torn message and never a
//! lost one. No lock is needed (so a crash can't wedge the board), and readers only ever see fully
//! written messages. Ordering is by filename (a fixed-width nanosecond timestamp, then pid+seq for
//! ties): stable on one host except under sub-millisecond clock-backwards, which at worst re-shows a
//! message once — benign for a coordination board.

use crate::blackboard::Message;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-process counter making each post's filename unique even within the same nanosecond.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Bound a posted body so the board stays readable (hygiene, not correctness — each message is its
/// own atomically-renamed file, so size never affects durability). Excerpted to this many chars.
const MAX_BODY: usize = 1500;

/// The shared on-disk board for one repo (session = repo).
pub struct FileBoard {
    dir: PathBuf,
}

impl FileBoard {
    /// The board for `repo` lives under `<repo>/.ensemble/board/`.
    pub fn open(repo: &Path) -> Self {
        Self {
            dir: repo.join(".ensemble").join("board"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Publish one message: write it to a temp file, then atomically rename it into the board dir so
    /// readers see it whole or not at all. Concurrent-safe across processes with no lock.
    pub fn post(&self, from: &str, kind: &str, body: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let msg = Message {
            from: from.to_string(),
            kind: kind.to_string(),
            body: excerpt(body, MAX_BODY),
        };
        let json = serde_json::to_string(&msg).map_err(std::io::Error::other)?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        // fixed-width fields → lexicographic filename order == chronological order (ties by pid,seq)
        let tmp = self.dir.join(format!(".tmp-{pid}-{seq}"));
        let final_path = self
            .dir
            .join(format!("{nanos:030}-{pid:010}-{seq:020}.json"));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.flush()?;
        }
        std::fs::rename(&tmp, &final_path)?; // atomic publish
        Ok(())
    }

    /// All messages at index ≥ `n`, in board order. Every `.json` is a complete message (atomic
    /// rename), so the cursor is stable; an unreadable/partial entry is skipped, never fatal. Empty
    /// if the board dir doesn't exist yet.
    pub fn read_since(&self, n: usize) -> std::io::Result<Vec<Message>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut names: Vec<OsString> = rd
            .flatten()
            .map(|e| e.file_name())
            .filter(|name| {
                Path::new(name)
                    .extension()
                    .map(|x| x == "json")
                    .unwrap_or(false)
            })
            .collect();
        names.sort();
        let msgs = names
            .into_iter()
            .skip(n)
            .filter_map(|name| {
                std::fs::read_to_string(self.dir.join(&name))
                    .ok()
                    .and_then(|s| serde_json::from_str::<Message>(&s).ok())
            })
            .collect();
        Ok(msgs)
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
        // post order is preserved (timestamp+seq filename ordering)
        assert_eq!(all[1].from, "claude");
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
        // two FileBoard handles on the same repo (≈ two processes) see each other's posts
        let tmp = tempfile::tempdir().unwrap();
        FileBoard::open(tmp.path()).post("a", "question", "anyone on auth?").unwrap();
        let seen = FileBoard::open(tmp.path()).read_since(0).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, "anyone on auth?");
    }

    #[test]
    fn concurrent_posts_all_survive_and_dont_corrupt() {
        // many threads posting at once: every message lands (atomic rename, no interleave) and each
        // is individually parseable.
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
