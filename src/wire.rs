use serde::{Deserialize, Serialize};

/// Orchestrator → node: run `agent` on `prompt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub agent: String,
    pub prompt: String,
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
}

impl RunResponse {
    pub fn ok(agent: &str, text: &str) -> Self {
        Self {
            ok: true,
            agent: agent.into(),
            text: text.into(),
            error: None,
            error_kind: None,
        }
    }
    pub fn err(agent: &str, kind: &str, msg: &str) -> Self {
        Self {
            ok: false,
            agent: agent.into(),
            text: String::new(),
            error: Some(msg.into()),
            error_kind: Some(kind.into()),
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
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: RunRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.agent, "codex");
        let ok = RunResponse::ok("codex", "done");
        assert!(ok.ok && ok.text == "done");
        let err = RunResponse::err("agy", "Empty", "no output");
        assert!(!err.ok && err.error_kind.as_deref() == Some("Empty"));
    }
}
