use crate::blackboard::Message;
use crate::supervise::ControlCmd;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLine {
    pub index: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorEvidence {
    pub name: String,
    pub repo: PathBuf,
    pub team: String,
    pub since: usize,
    pub stream_next: usize,
    pub board_next: usize,
    pub stream: Vec<EvidenceLine>,
    pub board: Vec<Message>,
    pub git_status: String,
    pub diff_summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorRecommendation {
    OnTrack,
    Steer,
    Abort,
    NeedsHuman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorReport {
    pub recommendation: SupervisorRecommendation,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer: Option<String>,
    #[serde(default)]
    pub critical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorApply {
    Advisory,
    ApplySteer,
    AbortOnCritical,
    ApplySteerAndAbortOnCritical,
}

pub fn build_supervisor_prompt(e: &SupervisorEvidence) -> String {
    let mut out = format!(
        "You are supervising local ensemble run `{}` for team `{}`.\n\
         Repo: {}\n\
         Stream cursor since: {}\n\
         Stream next cursor: {}\n\
         Board next cursor: {}\n\n\
         Recent stream evidence:\n",
        e.name,
        e.team,
        e.repo.display(),
        e.since,
        e.stream_next,
        e.board_next
    );
    if e.stream.is_empty() {
        out.push_str("- (no stream events)\n");
    } else {
        for line in &e.stream {
            out.push_str(&format!("- [{}] {}\n", line.index, line.text));
        }
    }

    out.push_str("\nRecent team board:\n");
    if e.board.is_empty() {
        out.push_str("- (no board messages)\n");
    } else {
        for m in &e.board {
            out.push_str(&format!("- {} [{}]: {}\n", m.from, m.kind, m.body));
        }
    }

    out.push_str("\nGit status:\n");
    out.push_str(if e.git_status.trim().is_empty() {
        "(clean)\n"
    } else {
        e.git_status.trim_end()
    });
    if !e.git_status.trim().is_empty() {
        out.push('\n');
    }

    out.push_str("\nDiff summary:\n");
    out.push_str(if e.diff_summary.trim().is_empty() {
        "(no diff)\n"
    } else {
        e.diff_summary.trim_end()
    });
    if !e.diff_summary.trim().is_empty() {
        out.push('\n');
    }

    out.push_str(
        "\nReturn exactly one JSON object and no markdown. Schema:\n\
         {\"recommendation\":\"on_track|steer|abort|needs_human\",\"reason\":\"short reason\",\"steer\":null,\"critical\":false}\n\
         Use on_track only when the run is clearly aligned. Use steer when a specific prompt should \
         redirect the next round; put that prompt in `steer`. Use abort only for an explicit critical \
         problem that should stop the run now and set `critical` to true. Use needs_human when evidence \
         is insufficient or ambiguous.\n",
    );
    out
}

pub fn parse_supervisor_report(text: &str) -> Result<SupervisorReport, String> {
    let json = extract_json_object(text)
        .ok_or_else(|| "supervisor output did not contain a JSON object".to_string())?;
    let mut report: SupervisorReport =
        serde_json::from_str(json).map_err(|e| format!("parse supervisor JSON: {e}"))?;
    report.reason = report.reason.trim().to_string();
    if report.reason.is_empty() {
        report.reason = "no reason provided".to_string();
    }
    report.steer = report
        .steer
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(report)
}

fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start < end).then_some(&trimmed[start..=end])
}

pub fn control_action_for_report(
    report: &SupervisorReport,
    apply: SupervisorApply,
    from: &str,
) -> Option<ControlCmd> {
    match report.recommendation {
        SupervisorRecommendation::Steer
            if matches!(
                apply,
                SupervisorApply::ApplySteer | SupervisorApply::ApplySteerAndAbortOnCritical
            ) =>
        {
            report.steer.as_deref().map(|prompt| ControlCmd::Steer {
                from: from.to_string(),
                prompt: prompt.to_string(),
            })
        }
        SupervisorRecommendation::Abort
            if report.critical
                && matches!(
                    apply,
                    SupervisorApply::AbortOnCritical
                        | SupervisorApply::ApplySteerAndAbortOnCritical
                ) =>
        {
            Some(ControlCmd::Abort {
                from: from.to_string(),
                hard: true,
            })
        }
        _ => None,
    }
}

pub fn collect_supervisor_evidence(
    repo: &std::path::Path,
    team: Option<&str>,
    name: &str,
    since: usize,
    limit: usize,
) -> std::io::Result<SupervisorEvidence> {
    let limit = limit.min(200);
    let stream_lines =
        crate::Feed::open(crate::member_stream_path(repo, name)).read_since(since)?;
    let stream: Vec<EvidenceLine> = stream_lines
        .iter()
        .take(limit)
        .enumerate()
        .map(|(offset, raw)| EvidenceLine {
            index: since + offset,
            text: crate::render_line(raw),
        })
        .collect();
    let stream_next = since + stream.len();

    let session =
        crate::team::resolve_team_session(repo, team, "supervisor", Some("supervisor"), None);
    let inbox = crate::team::read_team_inbox(&session, 0)?;
    let board_len = inbox.messages.len();
    let board = if board_len > limit {
        inbox.messages[board_len - limit..].to_vec()
    } else {
        inbox.messages
    };

    Ok(SupervisorEvidence {
        name: name.to_string(),
        repo: repo.to_path_buf(),
        team: session.team,
        since,
        stream_next,
        board_next: board_len,
        stream,
        board,
        git_status: git_capture(repo, &["status", "--short"]),
        diff_summary: git_capture(repo, &["diff", "--stat", "--no-ext-diff"]),
    })
}

fn git_capture(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output();
    match out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.is_empty() {
                format!("git {} failed with status {}", args.join(" "), out.status)
            } else {
                stderr
            }
        }
        Err(e) => format!("git {} unavailable: {e}", args.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> SupervisorEvidence {
        SupervisorEvidence {
            name: "team-phase1".to_string(),
            repo: PathBuf::from("/repo"),
            team: "ops".to_string(),
            since: 3,
            stream_next: 5,
            board_next: 2,
            stream: vec![
                EvidenceLine {
                    index: 3,
                    text: "turn #1 start implement task".to_string(),
                },
                EvidenceLine {
                    index: 4,
                    text: "tool bash cargo test failed".to_string(),
                },
            ],
            board: vec![Message {
                from: "codex@host".to_string(),
                kind: "result".to_string(),
                body: "implemented launcher".to_string(),
            }],
            git_status: " M src/main.rs".to_string(),
            diff_summary: " src/main.rs | 12 +++++".to_string(),
        }
    }

    #[test]
    fn supervisor_prompt_contains_bounded_evidence_and_json_contract() {
        let prompt = build_supervisor_prompt(&evidence());

        assert!(prompt.contains("team-phase1"));
        assert!(prompt.contains("Recent stream evidence"));
        assert!(prompt.contains("[3] turn #1 start implement task"));
        assert!(prompt.contains("Recent team board"));
        assert!(prompt.contains("codex@host [result]: implemented launcher"));
        assert!(prompt.contains("Git status"));
        assert!(prompt.contains(" M src/main.rs"));
        assert!(prompt.contains("Diff summary"));
        assert!(prompt.contains("src/main.rs | 12"));
        assert!(prompt.contains(r#""recommendation""#));
        assert!(prompt.contains("on_track"));
        assert!(prompt.contains("needs_human"));
    }

    #[test]
    fn parses_supervisor_json_even_when_wrapped_in_text() {
        let report = parse_supervisor_report(
            "analysis...\n```json\n{\"recommendation\":\"steer\",\"reason\":\"drifting\",\"steer\":\"Focus on Task 6\",\"critical\":false}\n```",
        )
        .unwrap();

        assert_eq!(report.recommendation, SupervisorRecommendation::Steer);
        assert_eq!(report.reason, "drifting");
        assert_eq!(report.steer.as_deref(), Some("Focus on Task 6"));
        assert!(!report.critical);
    }

    #[test]
    fn parses_all_recommendation_kinds() {
        for (raw, expected) in [
            ("on_track", SupervisorRecommendation::OnTrack),
            ("steer", SupervisorRecommendation::Steer),
            ("abort", SupervisorRecommendation::Abort),
            ("needs_human", SupervisorRecommendation::NeedsHuman),
        ] {
            let report =
                parse_supervisor_report(&format!(r#"{{"recommendation":"{raw}","reason":"ok"}}"#))
                    .unwrap();
            assert_eq!(report.recommendation, expected);
        }
    }

    #[test]
    fn control_action_is_advisory_unless_policy_allows_mutation() {
        let report = SupervisorReport {
            recommendation: SupervisorRecommendation::Steer,
            reason: "drift".to_string(),
            steer: Some("stay on Task 6".to_string()),
            critical: false,
        };

        assert_eq!(
            control_action_for_report(&report, SupervisorApply::Advisory, "supervisor"),
            None
        );
        assert_eq!(
            control_action_for_report(&report, SupervisorApply::ApplySteer, "supervisor"),
            Some(ControlCmd::Steer {
                from: "supervisor".to_string(),
                prompt: "stay on Task 6".to_string(),
            })
        );
    }

    #[test]
    fn abort_action_requires_abort_recommendation_and_critical_flag() {
        let noncritical = SupervisorReport {
            recommendation: SupervisorRecommendation::Abort,
            reason: "maybe bad".to_string(),
            steer: None,
            critical: false,
        };
        assert_eq!(
            control_action_for_report(&noncritical, SupervisorApply::AbortOnCritical, "supervisor",),
            None
        );

        let critical = SupervisorReport {
            recommendation: SupervisorRecommendation::Abort,
            reason: "dangerous command loop".to_string(),
            steer: None,
            critical: true,
        };
        assert_eq!(
            control_action_for_report(&critical, SupervisorApply::AbortOnCritical, "supervisor"),
            Some(ControlCmd::Abort {
                from: "supervisor".to_string(),
                hard: true,
            })
        );
    }

    #[test]
    fn collect_evidence_reads_stream_board_and_git_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let session = crate::team::resolve_team_session(
            tmp.path(),
            Some("ops"),
            "supervisor",
            Some("supervisor"),
            None,
        );
        crate::team::post_team_message(&session, "operator", "note", "check the run").unwrap();
        let feed = crate::Feed::open(crate::member_stream_path(tmp.path(), "team-phase1"));
        feed.append(r#"{"ev":"turn_start","n":1,"prompt":"do task","ts":"T"}"#)
            .unwrap();
        feed.append(r#"{"from":"codex","kind":"result","body":"made progress"}"#)
            .unwrap();

        let evidence =
            collect_supervisor_evidence(tmp.path(), Some("ops"), "team-phase1", 0, 10).unwrap();

        assert_eq!(evidence.name, "team-phase1");
        assert_eq!(evidence.team, "ops");
        assert_eq!(evidence.stream_next, 2);
        assert_eq!(evidence.stream.len(), 2);
        assert!(evidence.stream[0].text.contains("turn #1 start"));
        assert_eq!(evidence.board_next, 1);
        assert_eq!(evidence.board[0].body, "check the run");
        assert!(
            evidence.git_status.contains("git status")
                || evidence.git_status.is_empty()
                || evidence.git_status.contains("fatal")
        );
        assert!(
            evidence.diff_summary.contains("git diff")
                || evidence.diff_summary.is_empty()
                || evidence.diff_summary.contains("fatal")
        );
    }
}
