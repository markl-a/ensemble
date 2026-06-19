use crate::adapter::{Adapter, AdapterError, AgentOutput};
use std::path::Path;
use std::process::Command;

/// Drives a vendor CLI headlessly by exec: `program <args...> <prompt>`, capturing stdout as the
/// reply. Per-vendor invocation contracts live in the constructors (design §4a). stdin is closed.
pub struct ExecAdapter {
    name: String,
    program: String,
    args: Vec<String>,
}

impl ExecAdapter {
    /// codex: `codex exec --skip-git-repo-check "<prompt>"`. (Phase-1 parses the final text from
    /// stdout; refine structured `--json` parsing in Phase 1b.)
    pub fn codex() -> Self {
        Self {
            name: "codex".into(),
            program: "codex".into(),
            args: vec!["exec".into(), "--skip-git-repo-check".into()],
        }
    }

    /// claude: `claude -p <prompt>` — prints the answer to stdout (headless).
    pub fn claude() -> Self {
        Self {
            name: "claude".into(),
            program: "claude".into(),
            args: vec!["-p".into()],
        }
    }

    /// opencode: `opencode run <prompt>`.
    pub fn opencode() -> Self {
        Self {
            name: "opencode".into(),
            program: "opencode".into(),
            args: vec!["run".into()],
        }
    }
}

impl Adapter for ExecAdapter {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        let out = Command::new(&self.program)
            .args(&self.args)
            .arg(prompt)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AdapterError::NotInstalled(self.program.clone()))
            }
            Err(e) => return Err(AdapterError::Flaked(e.to_string())),
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.to_lowercase().contains("rate") || err.contains("429") {
                return Err(AdapterError::RateLimited);
            }
            return Err(AdapterError::Empty);
        }
        Ok(AgentOutput {
            agent: self.name.clone(),
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_programs() {
        assert_eq!(ExecAdapter::codex().name(), "codex");
        assert_eq!(ExecAdapter::claude().name(), "claude");
        assert_eq!(ExecAdapter::opencode().name(), "opencode");
    }
}
