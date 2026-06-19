use serde::Deserialize;
use std::collections::HashMap;

/// What to do when a reviewer agent flakes. `Exclude` drops it from the quorum with a logged
/// reason (never fake a pass). `Retry` re-runs the same agent once; `Substitute` falls back to the
/// agent's configured backup. In every case a verdict only enters the quorum from a real
/// successful run — a flake is never counted as approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFlake {
    Exclude,
    Retry,
    Substitute,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatePolicy {
    pub min_approvals: u32,
    pub max_rounds: u32,
    #[serde(deserialize_with = "de_on_flake")]
    pub on_flake: OnFlake,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoleConfig {
    pub agent: String,
    #[serde(default)]
    pub blind: bool,
}

/// Per-agent overrides. `backup` names the agent to substitute when this agent flakes and the
/// gate's `on_flake = "substitute"`. `node` is the base URL of a remote `ensemble serve` host that
/// runs this agent (e.g. "http://acer.tail.ts.net:7878") — when set, the orchestrator drives the
/// agent on that node over HTTP instead of locally.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub backup: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CrewConfig {
    pub gate: GatePolicy,
    pub pipeline: Vec<String>,
    pub roles: HashMap<String, RoleConfig>,
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
}

fn de_on_flake<'de, D: serde::Deserializer<'de>>(d: D) -> Result<OnFlake, D::Error> {
    let s = String::deserialize(d)?;
    match s.as_str() {
        "exclude" => Ok(OnFlake::Exclude),
        "retry" => Ok(OnFlake::Retry),
        "substitute" => Ok(OnFlake::Substitute),
        other => Err(serde::de::Error::custom(format!(
            "on_flake = \"{other}\" is not supported (use \"exclude\", \"retry\", or \"substitute\")"
        ))),
    }
}

/// Error from loading a crew config: a TOML parse failure, or a semantic-validation failure
/// (e.g. an empty `pipeline`, which would otherwise panic the conductor at `pipeline[0]`).
#[derive(Debug, thiserror::Error)]
pub enum CrewError {
    #[error("crew config parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("crew config invalid: {0}")]
    Invalid(String),
}

impl CrewConfig {
    pub fn from_toml(s: &str) -> Result<Self, CrewError> {
        let c: CrewConfig = toml::from_str(s)?;
        // A valid-but-empty pipeline parses fine but would panic the conductor at `pipeline[0]`
        // (the implementer). Reject it here so every real entry point (from_path → the CLI) fails
        // cleanly instead of panicking on malformed input.
        if c.pipeline.is_empty() {
            return Err(CrewError::Invalid(
                "pipeline must have at least one role (the implementer)".to_string(),
            ));
        }
        Ok(c)
    }
    pub fn from_path(p: &std::path::Path) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(p)?;
        Self::from_toml(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
    /// First pipeline role = the implementer.
    pub fn implementer_role(&self) -> &str {
        &self.pipeline[0]
    }
    /// All pipeline roles after the implementer = reviewers (their verdicts feed the gate).
    pub fn reviewer_roles(&self) -> Vec<&str> {
        self.pipeline.iter().skip(1).map(|s| s.as_str()).collect()
    }
    /// The backup agent configured for `agent` (used when `on_flake = "substitute"`), if any.
    pub fn backup_for(&self, agent: &str) -> Option<&str> {
        self.agents.get(agent).and_then(|a| a.backup.as_deref())
    }
    /// The remote node base URL configured for `agent` (a `[agents.<n>] node = "http://..."`),
    /// if any. When set, the orchestrator drives `agent` on that node via a `RemoteAdapter`.
    pub fn node_for(&self, agent: &str) -> Option<&str> {
        self.agents.get(agent).and_then(|a| a.node.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipeline_roles_and_gate() {
        let toml = r#"
            pipeline = ["implement", "review", "debug"]
            [gate]
            min_approvals = 2
            max_rounds = 2
            on_flake = "exclude"
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
            blind = true
            [roles.debug]
            agent = "agy"
        "#;
        let c = CrewConfig::from_toml(toml).unwrap();
        assert_eq!(c.pipeline, vec!["implement", "review", "debug"]);
        assert_eq!(c.gate.min_approvals, 2);
        assert_eq!(c.gate.max_rounds, 2);
        assert!(matches!(c.gate.on_flake, OnFlake::Exclude));
        assert_eq!(c.roles["implement"].agent, "codex");
        assert!(c.roles["review"].blind);
        assert!(!c.roles["debug"].blind);
        // implementer = first pipeline role; reviewers = the rest
        assert_eq!(c.implementer_role(), "implement");
        assert_eq!(c.reviewer_roles(), vec!["review", "debug"]);
    }

    #[test]
    fn rejects_empty_pipeline_so_conductor_cannot_panic() {
        let toml = r#"
            pipeline = []
            [gate]
            min_approvals = 1
            max_rounds = 1
            on_flake = "exclude"
        "#;
        assert!(
            CrewConfig::from_toml(toml).is_err(),
            "empty pipeline must be rejected"
        );
    }

    #[test]
    fn parses_all_on_flake_and_agent_backups() {
        let toml = r#"
            pipeline = ["implement", "review"]
            [gate]
            min_approvals = 1
            max_rounds = 1
            on_flake = "substitute"
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
            [agents.claude]
            backup = "opencode"
        "#;
        let c = CrewConfig::from_toml(toml).unwrap();
        assert!(matches!(c.gate.on_flake, OnFlake::Substitute));
        assert_eq!(c.backup_for("claude"), Some("opencode"));
        assert_eq!(c.backup_for("codex"), None);
    }

    #[test]
    fn rejects_unknown_on_flake() {
        let toml = r#"
            pipeline = ["implement", "review"]
            [gate]
            min_approvals = 1
            max_rounds = 1
            on_flake = "teleport"
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
        "#;
        assert!(CrewConfig::from_toml(toml).is_err());
    }

    #[test]
    fn parses_remote_agent_node_url() {
        let toml = r#"
            pipeline = ["implement","review"]
            [gate]
            min_approvals = 1
            max_rounds = 1
            on_flake = "exclude"
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
            [agents.claude]
            node = "http://acer.tail.ts.net:7878"
        "#;
        let c = CrewConfig::from_toml(toml).unwrap();
        assert_eq!(c.node_for("claude"), Some("http://acer.tail.ts.net:7878"));
        assert_eq!(c.node_for("codex"), None);
    }

    #[test]
    fn parses_repo_example() {
        let p = std::path::Path::new("examples/crew.toml");
        let c = CrewConfig::from_path(p).expect("examples/crew.toml must parse");
        assert_eq!(c.implementer_role(), "implement");
        assert_eq!(c.pipeline, vec!["implement", "review", "debug"]);
        assert_eq!(c.roles["implement"].agent, "codex");
        assert!(matches!(c.gate.on_flake, OnFlake::Exclude));
    }
}
