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
            // The implementer must actually edit files autonomously, so bypass approvals/sandbox
            // (it runs inside an isolated, throwaway git worktree). --skip-git-repo-check lets it
            // run in the worktree without a git-root prompt.
            args: vec![
                "exec".into(),
                "--dangerously-bypass-approvals-and-sandbox".into(),
                "--skip-git-repo-check".into(),
            ],
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

impl ExecAdapter {
    /// Build the base command. On Windows the vendor CLIs are typically npm-global `.cmd` shims
    /// (codex/opencode) that `CreateProcess` cannot exec directly, so route through `cmd /C
    /// <program> <args...>`; on Unix, exec the program directly. (Same lesson as a real PTY/CLI
    /// driver — npm shims need a shell on Windows.)
    #[cfg(windows)]
    fn build_command(&self) -> Command {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(&self.program).args(&self.args);
        c
    }
    #[cfg(not(windows))]
    fn build_command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c
    }
}

impl Adapter for ExecAdapter {
    fn name(&self) -> &str {
        &self.name
    }
    fn run(&self, prompt: &str, cwd: &Path) -> Result<AgentOutput, AdapterError> {
        use std::io::Write;
        // Pass the prompt via STDIN, not as a CLI arg. On Windows the prompt would otherwise go
        // through `cmd /C <program> ... "<prompt>"`, where cmd's command-line parsing MANGLES
        // multi-line / quoted prompts (newlines are command separators; quoting differs from the
        // CRT rules Rust applies). codex (`exec`), claude (`-p`) and opencode (`run`) all read the
        // prompt from stdin when given no positional prompt — verified on z13.
        let mut child = match self
            .build_command()
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AdapterError::NotInstalled(self.program.clone()))
            }
            Err(e) => return Err(AdapterError::Flaked(e.to_string())),
        };
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort: a broken pipe (CLI exited before reading) shows up as empty output
            // below. Dropping `stdin` closes it (EOF), so the CLI stops reading and proceeds.
            let _ = stdin.write_all(prompt.as_bytes());
        }
        let out = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => return Err(AdapterError::Flaked(e.to_string())),
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            // When shelled through `cmd /C`, a missing CLI is a non-zero exit + a "not recognized"
            // stderr (not a spawn NotFound), so classify that here.
            let err = String::from_utf8_lossy(&out.stderr);
            let err_low = err.to_lowercase();
            if err_low.contains("not recognized") || err_low.contains("cannot find") {
                return Err(AdapterError::NotInstalled(self.program.clone()));
            }
            if err_low.contains("rate") || err.contains("429") {
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
