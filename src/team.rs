use crate::blackboard::Message;
use crate::control_plane::{ControlPlane, LocalControlPlane};
use crate::ledger::Counts;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_TEAM: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamSession {
    pub repo: PathBuf,
    pub team: String,
    pub member: String,
    pub root: PathBuf,
    pub board: PathBuf,
    pub ledger: PathBuf,
    pub stream: PathBuf,
    pub control: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamLedgerCounts {
    pub queued: usize,
    pub claimed: usize,
    pub done: usize,
    pub failed: usize,
}

impl From<Counts> for TeamLedgerCounts {
    fn from(c: Counts) -> Self {
        Self {
            queued: c.queued,
            claimed: c.claimed,
            done: c.done,
            failed: c.failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInbox {
    pub messages: Vec<Message>,
    pub next: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatus {
    pub repo: PathBuf,
    pub team: String,
    pub root: PathBuf,
    pub board: PathBuf,
    pub board_len: usize,
    pub ledger: PathBuf,
    pub ledger_counts: TeamLedgerCounts,
    pub streams: Vec<String>,
    pub controls: Vec<String>,
}

pub fn default_team_name(team: Option<&str>) -> String {
    team.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TEAM)
        .to_string()
}

pub fn default_member_name(client: &str, raw_host: Option<&str>) -> String {
    let client = tame_client(client);
    let short = raw_host
        .map(|h| {
            h.trim()
                .split('.')
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty());
    match short {
        Some(h) => format!("{client}@{h}"),
        None => client,
    }
}

pub fn member_file_stem(member: &str) -> String {
    crate::journal::sanitize_slug(member)
}

pub fn team_root(repo: &Path, team: &str) -> PathBuf {
    let team = default_team_name(Some(team));
    if team == DEFAULT_TEAM {
        repo.join(".ensemble")
    } else {
        repo.join(".ensemble")
            .join("teams")
            .join(member_file_stem(&team))
    }
}

pub fn resolve_team_session(
    repo: &Path,
    team: Option<&str>,
    client: &str,
    explicit_member: Option<&str>,
    raw_host: Option<&str>,
) -> TeamSession {
    let team = default_team_name(team);
    let member = explicit_member
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_member_name(client, raw_host));
    let root = team_root(repo, &team);
    let stem = member_file_stem(&member);
    TeamSession {
        repo: repo.to_path_buf(),
        team,
        member,
        board: root.join("board.jsonl"),
        ledger: root.join("ledger.db"),
        stream: root.join("stream").join(format!("{stem}.ndjson")),
        control: root.join("control").join(format!("{stem}.ndjson")),
        root,
    }
}

pub fn post_team_message(
    session: &TeamSession,
    from: &str,
    kind: &str,
    body: &str,
) -> io::Result<usize> {
    LocalControlPlane::new().post_team_message(session, from, kind, body)
}

pub fn read_team_inbox(session: &TeamSession, since: usize) -> io::Result<TeamInbox> {
    LocalControlPlane::new().read_team_inbox(session, since)
}

pub fn team_status(session: &TeamSession) -> io::Result<TeamStatus> {
    LocalControlPlane::new().team_status(session)
}

pub fn render_team_inbox(inbox: &TeamInbox) -> String {
    if inbox.messages.is_empty() {
        return format!("no messages (next={})", inbox.next);
    }
    let mut out = String::new();
    for m in &inbox.messages {
        out.push_str(&format!("{} [{}]: {}\n", m.from, m.kind, m.body));
    }
    out.push_str(&format!("next={}", inbox.next));
    out
}

pub fn render_team_status(status: &TeamStatus) -> String {
    format!(
        "team={} repo={} board_len={} queued={} claimed={} done={} failed={} streams={} controls={}",
        status.team,
        status.repo.display(),
        status.board_len,
        status.ledger_counts.queued,
        status.ledger_counts.claimed,
        status.ledger_counts.done,
        status.ledger_counts.failed,
        status.streams.len(),
        status.controls.len()
    )
}

fn tame_client(client: &str) -> String {
    let cleaned: String = client
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "member".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_team_is_default_and_uses_existing_ensemble_root() {
        let repo = Path::new("/repo");
        assert_eq!(default_team_name(None), "default");
        assert_eq!(default_team_name(Some("")), "default");
        assert_eq!(team_root(repo, "default"), repo.join(".ensemble"));
    }

    #[test]
    fn non_default_team_is_confined_to_one_safe_component() {
        let repo = Path::new("/repo");
        let root = team_root(repo, "../../ops team");
        assert_eq!(root.parent().unwrap(), repo.join(".ensemble").join("teams"));
        assert!(!root.components().any(|c| c.as_os_str() == ".."));
    }

    #[test]
    fn default_member_name_supports_all_local_clients_including_agy() {
        assert_eq!(
            default_member_name("codex", Some("YOYOGOOD.local")),
            "codex@yoyogood"
        );
        assert_eq!(default_member_name("claude", Some("z13")), "claude@z13");
        assert_eq!(
            default_member_name("opencode", Some("my box!")),
            "opencode@mybox"
        );
        assert_eq!(default_member_name("agy", Some("AYANEO")), "agy@ayaneo");
        assert_eq!(default_member_name("agy", Some("...")), "agy");
    }

    #[test]
    fn member_file_stem_confines_hostile_member_names() {
        assert_eq!(member_file_stem("lead@z13"), "lead-z13");
        assert!(!member_file_stem("../../etc/passwd")
            .split(std::path::MAIN_SEPARATOR)
            .any(|part| part == ".."));
    }

    #[test]
    fn resolve_team_session_collects_stable_paths_and_explicit_member_wins() {
        let repo = Path::new("/repo");
        let s = resolve_team_session(
            repo,
            Some("ops"),
            "codex",
            Some("lead@z13"),
            Some("ignored-host"),
        );
        assert_eq!(s.repo, repo);
        assert_eq!(s.team, "ops");
        assert_eq!(s.member, "lead@z13");
        assert_eq!(s.root, repo.join(".ensemble").join("teams").join("ops"));
        assert_eq!(s.board, s.root.join("board.jsonl"));
        assert_eq!(s.ledger, s.root.join("ledger.db"));
        assert_eq!(s.stream, s.root.join("stream").join("lead-z13.ndjson"));
        assert_eq!(s.control, s.root.join("control").join("lead-z13.ndjson"));
    }

    #[test]
    fn team_message_roundtrip_uses_the_resolved_team_board() {
        let tmp = tempfile::tempdir().unwrap();
        let s = resolve_team_session(tmp.path(), Some("ops"), "codex", Some("codex@host"), None);

        let cursor = post_team_message(&s, "operator", "note", "hello team").unwrap();
        assert_eq!(cursor, 1);

        let inbox = read_team_inbox(&s, 0).unwrap();
        assert_eq!(inbox.next, 1);
        assert_eq!(inbox.messages.len(), 1);
        assert_eq!(inbox.messages[0].from, "operator");
        assert_eq!(inbox.messages[0].kind, "note");
        assert_eq!(inbox.messages[0].body, "hello team");
        assert!(s.board.exists());
        assert!(
            !tmp.path().join(".ensemble").join("board.jsonl").exists(),
            "non-default teams must not write to the default board"
        );
    }

    #[test]
    fn team_inbox_json_shape_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let s = resolve_team_session(tmp.path(), None, "codex", None, Some("host"));
        post_team_message(&s, "operator", "note", "online").unwrap();

        let inbox = read_team_inbox(&s, 0).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&inbox).unwrap()).unwrap();

        assert_eq!(v["next"], 1);
        assert_eq!(v["messages"][0]["from"], "operator");
        assert_eq!(v["messages"][0]["kind"], "note");
        assert_eq!(v["messages"][0]["body"], "online");
    }

    #[test]
    fn team_status_reports_zeroes_without_creating_missing_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let s = resolve_team_session(tmp.path(), None, "codex", None, Some("host"));

        let status = team_status(&s).unwrap();

        assert_eq!(status.repo, tmp.path());
        assert_eq!(status.team, "default");
        assert_eq!(status.board_len, 0);
        assert_eq!(status.ledger_counts.queued, 0);
        assert_eq!(status.ledger_counts.claimed, 0);
        assert_eq!(status.ledger_counts.done, 0);
        assert_eq!(status.ledger_counts.failed, 0);
        assert!(status.streams.is_empty());
        assert!(status.controls.is_empty());
        assert!(
            !s.ledger.exists(),
            "read-only status should not create an empty ledger just to report zeroes"
        );
    }

    #[test]
    fn team_status_counts_board_ledger_and_known_feeds() {
        let tmp = tempfile::tempdir().unwrap();
        let s = resolve_team_session(tmp.path(), None, "codex", None, Some("host"));
        post_team_message(&s, "operator", "note", "online").unwrap();
        crate::Ledger::open(&s.ledger)
            .unwrap()
            .enqueue("task-1", "demo", 1)
            .unwrap();
        std::fs::create_dir_all(s.root.join("stream")).unwrap();
        std::fs::create_dir_all(s.root.join("control")).unwrap();
        std::fs::write(s.root.join("stream").join("codex-host.ndjson"), "{}\n").unwrap();
        std::fs::write(s.root.join("control").join("run-1.ndjson"), "{}\n").unwrap();

        let status = team_status(&s).unwrap();

        assert_eq!(status.board_len, 1);
        assert_eq!(status.ledger_counts.queued, 1);
        assert_eq!(status.streams, vec!["codex-host".to_string()]);
        assert_eq!(status.controls, vec!["run-1".to_string()]);

        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(v["team"], "default");
        assert_eq!(v["boardLen"], 1);
        assert_eq!(v["ledgerCounts"]["queued"], 1);
        assert_eq!(v["streams"][0], "codex-host");
    }
}
