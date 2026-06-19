use serde::Deserialize;
use std::collections::HashMap;

/// What to do when a reviewer agent flakes. Phase 1 implements only `Exclude` (drop it from the
/// quorum with a logged reason — never fake a pass). `Retry`/`Substitute` are Phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFlake {
    Exclude,
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

#[derive(Debug, Clone, Deserialize)]
pub struct CrewConfig {
    pub gate: GatePolicy,
    pub pipeline: Vec<String>,
    pub roles: HashMap<String, RoleConfig>,
}

fn de_on_flake<'de, D: serde::Deserializer<'de>>(d: D) -> Result<OnFlake, D::Error> {
    let s = String::deserialize(d)?;
    match s.as_str() {
        "exclude" => Ok(OnFlake::Exclude),
        other => Err(serde::de::Error::custom(format!(
            "on_flake = \"{other}\" is not supported in Phase 1 (only \"exclude\")"
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
    fn rejects_unknown_on_flake_in_phase1() {
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
        "#;
        assert!(CrewConfig::from_toml(toml).is_err());
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
