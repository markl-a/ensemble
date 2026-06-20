use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

/// What an agent produced on one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutput {
    pub agent: String,
    pub text: String,
}

/// Why an agent did NOT produce a usable answer. These are the degrade signals: the gate
/// must treat any of them as "this reviewer is unavailable", never as approval.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("agent flaked: {0}")]
    Flaked(String),
    #[error("agent produced empty output")]
    Empty,
    #[error("agent rate-limited / quota exhausted")]
    RateLimited,
    #[error("agent CLI not installed: {0}")]
    NotInstalled(String),
}

impl AdapterError {
    /// A DISTINCT, stable process exit code per failure kind, so a conductor's shell (e.g. Claude
    /// Code's Bash tool) can branch on *why* a delegated `ensemble agent` run failed. 0=ok and
    /// 7=no-adapter-resolved are owned by main.rs and never overlap these.
    pub fn exit_code(&self) -> i32 {
        match self {
            AdapterError::Flaked(_) => 3,
            AdapterError::Empty => 4,
            AdapterError::RateLimited => 5,
            AdapterError::NotInstalled(_) => 6,
        }
    }
}

/// A vendor AI CLI driven headlessly. Implementors encode the per-vendor invocation contract.
pub trait Adapter: Send + Sync {
    /// The agent's name as referenced in crew.toml (e.g. "codex", "claude").
    fn name(&self) -> &str;
    /// Run one turn: hand `prompt` to the agent with working dir `cwd`, return its reply.
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError>;
}

/// A scripted adapter for hermetic tests: returns successive queued responses; an exhausted
/// queue yields `AdapterError::Empty` so tests can model an agent that stops responding.
pub struct MockAdapter {
    name: String,
    responses: Mutex<VecDeque<Result<String, AdapterError>>>,
}

impl MockAdapter {
    pub fn new(name: &str, responses: Vec<Result<String, AdapterError>>) -> Self {
        Self {
            name: name.to_string(),
            responses: Mutex::new(responses.into()),
        }
    }
}

impl Adapter for MockAdapter {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, _prompt: &str, _cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let mut q = self.responses.lock().unwrap();
        match q.pop_front() {
            Some(Ok(text)) => Ok(AgentOutput {
                agent: self.name.clone(),
                text,
            }),
            Some(Err(e)) => Err(e),
            None => Err(AdapterError::Empty),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn mock_returns_scripted_then_flakes() {
        let m = MockAdapter::new(
            "codex",
            vec![
                Ok("I implemented the change.".to_string()),
                Err(AdapterError::Empty),
            ],
        );
        assert_eq!(m.name(), "codex");
        let out = m.run("do the thing", Path::new(".")).unwrap();
        assert_eq!(out.agent, "codex");
        assert_eq!(out.text, "I implemented the change.");
        assert!(matches!(
            m.run("again", Path::new(".")),
            Err(AdapterError::Empty)
        ));
    }

    #[test]
    fn mock_exhausted_returns_empty() {
        let m = MockAdapter::new("claude", vec![]);
        assert!(matches!(
            m.run("x", Path::new(".")),
            Err(AdapterError::Empty)
        ));
    }

    #[test]
    fn exit_code_is_total_and_distinct() {
        // Every AdapterError variant maps to a DISTINCT non-zero code so a conductor's shell can
        // branch on the failure kind. (0 = ok is owned by main.rs, not an error variant.)
        let codes = [
            AdapterError::Flaked("x".into()).exit_code(),
            AdapterError::Empty.exit_code(),
            AdapterError::RateLimited.exit_code(),
            AdapterError::NotInstalled("x".into()).exit_code(),
        ];
        assert_eq!(codes, [3, 4, 5, 6]);
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "exit codes must be distinct");
        assert!(codes.iter().all(|&c| c != 0 && c != 7));
    }
}
