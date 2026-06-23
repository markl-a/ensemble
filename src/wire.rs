use serde::{Deserialize, Serialize};

use crate::supervise::ControlCmd;
use crate::team::{TeamInbox, TeamStatus};

/// Orchestrator → node: run `agent` on `prompt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub agent: String,
    pub prompt: String,
    /// Phase 3b-1 git-sync context. Absent (`None`) ⇒ the node runs in its own checkout (Phase 3a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoCtx>,
}

/// Git-sync context attached to a `/run` request (Phase 3b-1). Carries the base commit as a git
/// bundle so the node can reproduce the orchestrator's state and return the agent's edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCtx {
    /// base commit bundle, base64-encoded (`git bundle create - <base_ref>`).
    pub base_bundle_b64: String,
    /// the ref the bundle's tip is recorded under (always "HEAD" in slice-1).
    pub base_ref: String,
    /// a unique id for this job; the node commits onto `dispatch/<job_id>`.
    pub job_id: String,
}

/// Git-sync result on a `/run` response: the `dispatch/<job_id>` branch the node committed the
/// agent's edits onto, as a base64 git bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoResult {
    pub result_bundle_b64: String,
    pub branch: String,
}

/// Node → orchestrator: the result of a `/run`. `ok` true ⇒ `text` is the answer; false ⇒
/// `error_kind` (one of Flaked|Empty|RateLimited|NotInstalled) + `error` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResponse {
    pub ok: bool,
    pub agent: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_kind: Option<String>,
    /// Phase 3b-1 git-sync result (the dispatch branch the node committed the agent's edits onto).
    /// Absent on a Phase-3a run or any error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_result: Option<RepoResult>,
}

/// Orchestrator → node: one control-plane operation against repo-local team state on that node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlPlaneRequest {
    TeamStatus {
        repo: String,
        team: String,
    },
    TeamSay {
        repo: String,
        team: String,
        from: String,
        kind: String,
        body: String,
    },
    TeamInbox {
        repo: String,
        team: String,
        since: usize,
    },
    Watch {
        repo: String,
        name: String,
        since: usize,
    },
    AppendControl {
        repo: String,
        name: String,
        cmd: ControlCmd,
    },
}

/// Node → orchestrator: result for a `/control` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TeamStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<TeamInbox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlPlaneResponse {
    pub fn ok_status(status: TeamStatus) -> Self {
        Self {
            ok: true,
            status: Some(status),
            inbox: None,
            stream: None,
            next: None,
            error_kind: None,
            error: None,
        }
    }

    pub fn ok_inbox(inbox: TeamInbox) -> Self {
        Self {
            ok: true,
            status: None,
            inbox: Some(inbox),
            stream: None,
            next: None,
            error_kind: None,
            error: None,
        }
    }

    pub fn ok_stream(stream: Vec<String>) -> Self {
        Self {
            ok: true,
            status: None,
            inbox: None,
            stream: Some(stream),
            next: None,
            error_kind: None,
            error: None,
        }
    }

    pub fn ok_next(next: usize) -> Self {
        Self {
            ok: true,
            status: None,
            inbox: None,
            stream: None,
            next: Some(next),
            error_kind: None,
            error: None,
        }
    }

    pub fn err(kind: &str, msg: &str) -> Self {
        Self {
            ok: false,
            status: None,
            inbox: None,
            stream: None,
            next: None,
            error_kind: Some(kind.into()),
            error: Some(msg.into()),
        }
    }
}

impl RunResponse {
    pub fn ok(agent: &str, text: &str) -> Self {
        Self {
            ok: true,
            agent: agent.into(),
            text: text.into(),
            error: None,
            error_kind: None,
            repo_result: None,
        }
    }
    /// Like `ok`, but carries the git-synced result branch back to the orchestrator.
    pub fn ok_with_repo(
        agent: &str,
        text: &str,
        result_bundle_b64: String,
        branch: String,
    ) -> Self {
        Self {
            ok: true,
            agent: agent.into(),
            text: text.into(),
            error: None,
            error_kind: None,
            repo_result: Some(RepoResult {
                result_bundle_b64,
                branch,
            }),
        }
    }
    pub fn err(agent: &str, kind: &str, msg: &str) -> Self {
        Self {
            ok: false,
            agent: agent.into(),
            text: String::new(),
            error: Some(msg.into()),
            error_kind: Some(kind.into()),
            repo_result: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn run_request_response_round_trip_json() {
        let req = RunRequest {
            agent: "codex".into(),
            prompt: "hi".into(),
            repo: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: RunRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.agent, "codex");
        let ok = RunResponse::ok("codex", "done");
        assert!(ok.ok && ok.text == "done");
        let err = RunResponse::err("agy", "Empty", "no output");
        assert!(!err.ok && err.error_kind.as_deref() == Some("Empty"));
    }

    #[test]
    fn repo_ctx_is_omitted_when_absent_and_round_trips_when_present() {
        let plain = RunRequest {
            agent: "codex".into(),
            prompt: "hi".into(),
            repo: None,
        };
        let s = serde_json::to_string(&plain).unwrap();
        assert!(
            !s.contains("repo"),
            "absent repo ctx must not appear on the wire"
        );
        let withrepo = RunRequest {
            agent: "codex".into(),
            prompt: "hi".into(),
            repo: Some(RepoCtx {
                base_bundle_b64: "AAA".into(),
                base_ref: "HEAD".into(),
                job_id: "codex-0".into(),
            }),
        };
        let back: RunRequest =
            serde_json::from_str(&serde_json::to_string(&withrepo).unwrap()).unwrap();
        assert_eq!(back.repo.unwrap().job_id, "codex-0");
        let r = RunResponse::ok_with_repo("codex", "done", "BBB".into(), "dispatch/codex-0".into());
        let back: RunResponse = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.repo_result.unwrap().branch, "dispatch/codex-0");
    }

    #[test]
    fn control_plane_request_response_round_trip_json() {
        let req = ControlPlaneRequest::AppendControl {
            repo: "/repo".into(),
            name: "claude@node".into(),
            cmd: ControlCmd::Steer {
                from: "operator".into(),
                prompt: "focus".into(),
            },
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains(r#""op":"append_control""#), "got {s}");
        let back: ControlPlaneRequest = serde_json::from_str(&s).unwrap();
        match back {
            ControlPlaneRequest::AppendControl { name, cmd, .. } => {
                assert_eq!(name, "claude@node");
                assert_eq!(
                    cmd,
                    ControlCmd::Steer {
                        from: "operator".into(),
                        prompt: "focus".into()
                    }
                );
            }
            other => panic!("expected append_control, got {other:?}"),
        }

        let ok = ControlPlaneResponse::ok_next(7);
        let back: ControlPlaneResponse =
            serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
        assert!(back.ok);
        assert_eq!(back.next, Some(7));
        assert!(back.error.is_none());

        let err = ControlPlaneResponse::err("BadRequest", "bad repo");
        assert!(!err.ok);
        assert_eq!(err.error_kind.as_deref(), Some("BadRequest"));
    }
}
