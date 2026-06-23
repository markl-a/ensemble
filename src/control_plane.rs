//! Shared team/control-plane boundary.
//!
//! Phase 1 stores team state in the local repo under `.ensemble/`. This module is the stable
//! interface used by CLI, MCP, and supervision code so a later remote/HTTP control plane can
//! implement the same operations without changing every caller.

use crate::blackboard::Message;
use crate::board::FileBoard;
use crate::ledger::Ledger;
use crate::ndjson::Feed;
use crate::supervise::{member_control_path, member_stream_path, ControlCmd};
use crate::team::{TeamInbox, TeamLedgerCounts, TeamSession, TeamStatus};
use std::io;
use std::path::Path;

/// The operations a member/operator needs to observe and steer a team.
pub trait ControlPlane {
    fn team_status(&self, session: &TeamSession) -> io::Result<TeamStatus>;
    fn post_team_message(
        &self,
        session: &TeamSession,
        from: &str,
        kind: &str,
        body: &str,
    ) -> io::Result<usize>;
    fn read_team_inbox(&self, session: &TeamSession, since: usize) -> io::Result<TeamInbox>;
    fn read_stream(&self, repo: &Path, name: &str, since: usize) -> io::Result<Vec<String>>;
    fn append_control(&self, repo: &Path, name: &str, cmd: &ControlCmd) -> io::Result<usize>;
}

/// Local repo-backed implementation. This is the Phase 1 transport.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalControlPlane;

impl LocalControlPlane {
    pub fn new() -> Self {
        Self
    }
}

impl ControlPlane for LocalControlPlane {
    fn team_status(&self, session: &TeamSession) -> io::Result<TeamStatus> {
        let board = FileBoard::open_at(&session.root);
        let board_len = board.len()?;
        let ledger_counts = if session.ledger.exists() {
            Ledger::open(&session.ledger)
                .map_err(|e| io::Error::other(e.to_string()))?
                .counts()
                .map_err(|e| io::Error::other(e.to_string()))?
                .into()
        } else {
            TeamLedgerCounts::default()
        };
        Ok(TeamStatus {
            repo: session.repo.clone(),
            team: session.team.clone(),
            root: session.root.clone(),
            board: session.board.clone(),
            board_len,
            ledger: session.ledger.clone(),
            ledger_counts,
            streams: list_feed_stems(&session.root.join("stream"))?,
            controls: list_feed_stems(&session.root.join("control"))?,
        })
    }

    fn post_team_message(
        &self,
        session: &TeamSession,
        from: &str,
        kind: &str,
        body: &str,
    ) -> io::Result<usize> {
        FileBoard::open_at(&session.root).post(from, kind, body)
    }

    fn read_team_inbox(&self, session: &TeamSession, since: usize) -> io::Result<TeamInbox> {
        let messages: Vec<Message> = FileBoard::open_at(&session.root).read_since(since)?;
        Ok(TeamInbox {
            next: since + messages.len(),
            messages,
        })
    }

    fn read_stream(&self, repo: &Path, name: &str, since: usize) -> io::Result<Vec<String>> {
        Feed::open(member_stream_path(repo, name)).read_since(since)
    }

    fn append_control(&self, repo: &Path, name: &str, cmd: &ControlCmd) -> io::Result<usize> {
        let feed = Feed::open(member_control_path(repo, name));
        let line = serde_json::to_string(cmd).map_err(io::Error::other)?;
        feed.append(&line)
    }
}

fn list_feed_stems(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_control_plane_roundtrips_team_board() {
        let tmp = tempfile::tempdir().unwrap();
        let session =
            crate::team::resolve_team_session(tmp.path(), Some("ops"), "codex", None, Some("z13"));
        let cp = LocalControlPlane::new();

        let cursor = cp
            .post_team_message(&session, "operator", "note", "hello")
            .unwrap();
        assert_eq!(cursor, 1);

        let inbox = cp.read_team_inbox(&session, 0).unwrap();
        assert_eq!(inbox.next, 1);
        assert_eq!(inbox.messages[0].from, "operator");
        assert_eq!(inbox.messages[0].kind, "note");
        assert_eq!(inbox.messages[0].body, "hello");
    }

    #[test]
    fn local_control_plane_reports_status_and_feeds() {
        let tmp = tempfile::tempdir().unwrap();
        let session =
            crate::team::resolve_team_session(tmp.path(), None, "codex", Some("codex@z13"), None);
        let cp = LocalControlPlane::new();
        cp.post_team_message(&session, "operator", "note", "online")
            .unwrap();
        crate::Ledger::open(&session.ledger)
            .unwrap()
            .enqueue("task-1", "demo", 1)
            .unwrap();
        Feed::open(crate::member_stream_path(tmp.path(), "codex@z13"))
            .append(r#"{"ev":"output","n":1,"text":"hi","ts":"T"}"#)
            .unwrap();
        cp.append_control(
            tmp.path(),
            "codex@z13",
            &ControlCmd::Steer {
                from: "operator".into(),
                prompt: "focus".into(),
            },
        )
        .unwrap();

        let status = cp.team_status(&session).unwrap();
        assert_eq!(status.board_len, 1);
        assert_eq!(status.ledger_counts.queued, 1);
        assert_eq!(status.streams, vec!["codex-z13".to_string()]);
        assert_eq!(status.controls, vec!["codex-z13".to_string()]);
    }

    #[test]
    fn local_control_plane_reads_stream_and_appends_control() {
        let tmp = tempfile::tempdir().unwrap();
        let cp = LocalControlPlane::new();
        Feed::open(crate::member_stream_path(tmp.path(), "run-1"))
            .append(r#"{"from":"codex","kind":"result","body":"done"}"#)
            .unwrap();

        let stream = cp.read_stream(tmp.path(), "run-1", 0).unwrap();
        assert_eq!(stream.len(), 1);

        let next = cp
            .append_control(
                tmp.path(),
                "run-1",
                &ControlCmd::Abort {
                    from: "operator".into(),
                    hard: true,
                },
            )
            .unwrap();
        assert_eq!(next, 1);
        let lines = Feed::open(crate::member_control_path(tmp.path(), "run-1"))
            .read_since(0)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ControlCmd>(&lines[0]).unwrap(),
            ControlCmd::Abort {
                from: "operator".into(),
                hard: true
            }
        );
    }
}
