//! A generic append-only NDJSON feed: one JSON object per line, multi-process safe. Backs the
//! live-supervision feeds (.ensemble/stream/<member>.ndjson, control/<member>.ndjson).
//!
//! This module is the SINGLE OWNER of the append-only file discipline (an `fs2` exclusive lock
//! serializes appends, a shared lock guards reads, a torn trailing line is repaired before the next
//! append, and the lossless cursor is the line count read back while the lock is still held). That
//! discipline is exposed as `append_line_locked` / `read_lines_locked`, each taking the caller's own
//! per-line validity predicate. `Feed` is the JSON-value flavor used here; `board::FileBoard` is the
//! `Message` flavor — both delegate to these helpers, so the locking code lives in exactly one place.

use fs2::FileExt;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// An append-only NDJSON feed at one path.
pub struct Feed {
    path: PathBuf,
}

impl Feed {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one JSON line under an EXCLUSIVE lock, creating the parent dir if needed, and return the
    /// feed CURSOR (count of valid JSON lines) read back while the lock is STILL HELD — so a poller of
    /// `read_since(cursor)` never skips a concurrently-appended line (the lossless-cursor guarantee from
    /// board.rs). Rejects a `line` that isn't a single valid JSON value (so we never persist a record we
    /// couldn't read back). A trailing newline on `line` is normalized; exactly one is written. An
    /// INTERIOR newline is rejected: a multi-line JSON value (e.g. `[\n1\n]` or a pretty object) is valid
    /// JSON yet would fracture into a different record when the reader splits on `\n` — so it must never
    /// reach disk (this also forbids smuggling two records, or a partial torn line, through one append).
    pub fn append(&self, line: &str) -> std::io::Result<usize> {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.contains('\n') || trimmed.contains('\r') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ndjson append: line contains an interior newline (would split into multiple records)",
            ));
        }
        if serde_json::from_str::<Value>(trimmed).is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ndjson append: line is not valid JSON",
            ));
        }
        let mut buf = trimmed.to_string();
        buf.push('\n');
        append_line_locked(&self.path, buf.as_bytes(), |l| {
            serde_json::from_slice::<Value>(l).is_ok()
        })
    }

    /// All valid JSON lines at index ≥ `n`, in append order, under a SHARED lock (so a concurrent
    /// append can't expose a half-written line). Torn/non-JSON lines are skipped individually. Empty if
    /// the feed doesn't exist yet.
    pub fn read_since(&self, n: usize) -> std::io::Result<Vec<String>> {
        Ok(read_lines_locked(&self.path)?
            .into_iter()
            .filter(|l| serde_json::from_slice::<Value>(l).is_ok())
            .map(|l| String::from_utf8_lossy(&l).into_owned())
            .skip(n)
            .collect())
    }

    /// Total valid-line count — the cursor a poller advances to.
    pub fn len(&self) -> std::io::Result<usize> {
        Ok(self.read_since(0)?.len())
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Append a single record's `line` bytes to the NDJSON file at `path` under an EXCLUSIVE advisory
/// lock (so concurrent appends from any process serialize), creating the parent dir if needed, and
/// return the feed CURSOR — the count of existing lines for which `valid` is true — read back while
/// the lock is STILL HELD, so polling `read_lines_locked` from that cursor never skips a concurrently
/// appended line. A torn trailing line is repaired first (see `append_terminated`). The caller owns
/// the per-record predicate (`Value` for `Feed`, `Message` for `FileBoard`) and must pass exactly one
/// record with no interior newline. This is the single home for the append/lock/cursor discipline.
pub(crate) fn append_line_locked<F: Fn(&[u8]) -> bool>(
    path: &Path,
    line: &[u8],
    valid: F,
) -> std::io::Result<usize> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    f.lock_exclusive()?; // blocks until no other writer holds it; released on unlock/close
    let r = append_terminated(&mut f, line).and_then(|()| count_lines(&mut f, &valid));
    let _ = f.unlock();
    r
}

/// Read every non-empty line of the NDJSON file at `path` under a SHARED lock (so a concurrent append
/// can't expose a half-written line), returning each line's raw bytes in append order. The caller
/// parses/filters and applies its own `skip(cursor)`. Empty if the file doesn't exist yet.
pub(crate) fn read_lines_locked(path: &Path) -> std::io::Result<Vec<Vec<u8>>> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    f.lock_shared()?;
    let mut bytes = Vec::new();
    let read = f.read_to_end(&mut bytes);
    let _ = f.unlock();
    read?;
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| l.to_vec())
        .collect())
}

/// Append `line` to `f` (held under an exclusive lock, opened read+append), repairing a torn tail
/// first: if the file is non-empty and does NOT end in `\n`, terminate the partial line before
/// appending so O_APPEND can't weld our record onto it.
fn append_terminated(f: &mut std::fs::File, line: &[u8]) -> std::io::Result<()> {
    let len = f.metadata()?.len();
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    f.write_all(line)
}

/// Count lines in `f` (held under the exclusive lock) for which `valid` is true, using the SAME
/// non-empty filter as `read_lines_locked` so the count equals the cursor a reader polls from. Seeks
/// to the start first (the caller left the position at EOF after appending).
fn count_lines<F: Fn(&[u8]) -> bool>(f: &mut std::fs::File, valid: &F) -> std::io::Result<usize> {
    f.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .filter(|l| valid(l))
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(dir: &std::path::Path, name: &str) -> Feed {
        Feed::open(dir.join(name))
    }

    #[test]
    fn append_then_read_roundtrips_with_a_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        assert!(f.is_empty().unwrap());
        let c1 = f.append(r#"{"ev":"a","n":1}"#).unwrap();
        let c2 = f.append(r#"{"ev":"b","n":2}"#).unwrap();
        assert_eq!((c1, c2), (1, 2), "cursor sits one past each own line");
        let all = f.read_since(0).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].contains(r#""ev":"a""#));
        assert!(all[1].contains(r#""ev":"b""#));
        assert_eq!(
            f.read_since(1).unwrap().len(),
            1,
            "cursor skips earlier lines"
        );
    }

    #[test]
    fn read_missing_feed_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "nope.ndjson");
        assert!(f.read_since(0).unwrap().is_empty());
        assert_eq!(f.len().unwrap(), 0);
    }

    #[test]
    fn append_rejects_non_json_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        assert!(f.append("not json").is_err());
        assert!(
            f.read_since(0).unwrap().is_empty(),
            "a rejected append must write nothing"
        );
    }

    #[test]
    fn append_rejects_interior_newline_so_one_record_stays_one_line() {
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        // These ARE valid JSON values, but they span lines. NDJSON splits on '\n', so on read-back each
        // would fracture into a DIFFERENT record (`[\n1\n]` → `1`, a pretty object → its inner string),
        // silently violating the one-record-per-line + cursor guarantees. They must be rejected outright.
        assert!(
            f.append("[\n1\n]").is_err(),
            "multi-line array must be rejected"
        );
        assert!(
            f.append("{\n\"ev\":\"x\"\n}").is_err(),
            "pretty object must be rejected"
        );
        assert!(
            f.append("{\"ev\":\"a\"}\r\n{\"ev\":\"b\"}").is_err(),
            "embedded CRLF must be rejected"
        );
        assert!(
            f.read_since(0).unwrap().is_empty(),
            "a rejected multi-line append must write nothing"
        );
        // a compact single-line value with a trailing newline is still fine (trailing-only is normalized)
        assert!(f.append("{\"ev\":\"ok\"}\n").is_ok());
        assert_eq!(f.read_since(0).unwrap().len(), 1);
    }

    #[test]
    fn torn_tail_is_repaired_so_the_next_append_is_not_swallowed() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        f.append(r#"{"ev":"first"}"#).unwrap();
        let mut raw = std::fs::OpenOptions::new()
            .append(true)
            .open(f.path())
            .unwrap();
        raw.write_all(br#"{"ev":"torn"#).unwrap(); // partial line, no newline
        drop(raw);
        assert_eq!(
            f.read_since(0).unwrap().len(),
            1,
            "the torn partial line is skipped"
        );
        f.append(r#"{"ev":"third"}"#).unwrap();
        let all = f.read_since(0).unwrap();
        assert_eq!(all.len(), 2, "torn line skipped; both real records kept");
        assert!(all[1].contains("third"));
    }

    #[test]
    fn a_bad_middle_line_is_skipped_without_hiding_later_lines() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let f = feed(tmp.path(), "s.ndjson");
        f.append(r#"{"ev":"one"}"#).unwrap();
        let mut raw = std::fs::OpenOptions::new()
            .append(true)
            .open(f.path())
            .unwrap();
        raw.write_all(&[0xff, 0xfe, b'\n']).unwrap(); // non-UTF-8 / non-JSON, newline-terminated
        drop(raw);
        f.append(r#"{"ev":"three"}"#).unwrap();
        let all = f.read_since(0).unwrap();
        assert_eq!(all.len(), 2, "bad middle line skipped: {all:?}");
        assert!(all[0].contains("one") && all[1].contains("three"));
    }

    #[test]
    fn concurrent_appends_serialize_with_no_loss() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::thread::scope(|s| {
            for i in 0..20 {
                let dir = dir.clone();
                s.spawn(move || {
                    Feed::open(dir.join("s.ndjson"))
                        .append(&format!(r#"{{"ev":"w","i":{i}}}"#))
                        .unwrap();
                });
            }
        });
        let all = Feed::open(dir.join("s.ndjson")).read_since(0).unwrap();
        assert_eq!(all.len(), 20, "every concurrent append survives");
    }
}
