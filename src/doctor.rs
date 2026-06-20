//! `ensemble doctor` — environment-readiness check. Reports, before a crew runs, whether each
//! thing the mesh needs is present: the four AI CLIs (codex/claude/opencode/agy), `tailscale` (for
//! cross-machine discovery), and whether the cwd is a git repo. The CORE (`check_tools` / `is_ready`)
//! is pure — it asks a `present(name)` probe / reads a `&[ToolStatus]` — so it is hermetically
//! testable; `run_checks` is the thin IO wrapper that supplies a real PATH probe + a git-repo check.

use std::path::Path;
use std::process::Command;

/// One readiness item: a named thing the mesh wants, whether it is present, and (when missing) a
/// one-line hint telling the operator how to satisfy it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatus {
    pub name: String,
    pub ok: bool,
    pub hint: String,
}

/// The AI CLIs the crew drives, paired with the hint shown when one is missing. Order is the order
/// the report prints them in.
const TOOLS: &[(&str, &str)] = &[
    ("codex", "install + log in: codex"),
    ("claude", "install + log in: claude (Claude Code)"),
    ("opencode", "install + log in: opencode"),
    ("agy", "install + log in: agy (antigravity)"),
];

/// The label `run_checks` files the cwd git-repo check under. Shared with `is_ready` so the
/// readiness rule and the report agree on the name.
const GIT_REPO: &str = "git-repo (cwd)";

/// PURE core: for each AI CLI ask `present(name)` and build its `ToolStatus`. A present tool gets an
/// empty hint; a missing one carries its install/login hint. Deterministic — no IO, no clock — so it
/// is fully unit-testable with a mock probe.
pub fn check_tools(present: impl Fn(&str) -> bool) -> Vec<ToolStatus> {
    TOOLS
        .iter()
        .map(|(name, hint)| {
            let ok = present(name);
            ToolStatus {
                name: name.to_string(),
                ok,
                hint: if ok { String::new() } else { hint.to_string() },
            }
        })
        .collect()
}

/// PURE: the mesh is minimally usable iff the cwd is a git repo (so edits can be bundled back) AND
/// at least one AI CLI is present (so a crew can actually run a turn). A missing `tailscale` or some
/// — but not all — CLIs are warnings, not failures, so they do NOT flip readiness. Drives the
/// `doctor` exit code, so a script can gate on `ensemble doctor`.
pub fn is_ready(statuses: &[ToolStatus]) -> bool {
    let git_ok = statuses.iter().any(|t| t.name == GIT_REPO && t.ok);
    let any_cli = TOOLS
        .iter()
        .any(|(name, _)| statuses.iter().any(|t| t.name == *name && t.ok));
    git_ok && any_cli
}

/// IO wrapper: run the real checks. Probes PATH for each AI CLI plus `tailscale`, and adds a
/// git-repo check for the cwd. The AI-CLI list and hints come from `check_tools` so the core stays
/// the single source of truth for what the mesh needs.
pub fn run_checks() -> Vec<ToolStatus> {
    let mut out = check_tools(on_path);
    let ts = on_path("tailscale");
    out.push(ToolStatus {
        name: "tailscale".to_string(),
        ok: ts,
        hint: if ts {
            String::new()
        } else {
            "optional: install tailscale for cross-machine discovery".to_string()
        },
    });
    let git = crate::repo_sync::is_git_worktree(Path::new("."));
    out.push(ToolStatus {
        name: GIT_REPO.to_string(),
        ok: git,
        hint: if git {
            String::new()
        } else {
            "run inside a git repo so a crew's edits can be bundled back".to_string()
        },
    });
    out
}

/// True if `tool` is resolvable on PATH. Uses `where` on Windows / `command -v` on Unix — a cheap
/// PATH lookup that does NOT spawn the tool itself (so an un-logged-in CLI still counts as present).
fn on_path(tool: &str) -> bool {
    let (prog, args) = if cfg!(windows) {
        ("cmd", vec!["/C", "where", tool])
    } else {
        ("sh", vec!["-c", "command -v \"$1\"", "sh", tool])
    };
    Command::new(prog)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_present_all_ok_no_hints() {
        let st = check_tools(|_| true);
        assert_eq!(st.len(), TOOLS.len());
        assert!(st.iter().all(|t| t.ok), "every tool should be ok");
        assert!(
            st.iter().all(|t| t.hint.is_empty()),
            "present tools carry no hint"
        );
        // the four expected CLIs are reported, in order
        let names: Vec<&str> = st.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["codex", "claude", "opencode", "agy"]);
    }

    #[test]
    fn one_missing_is_not_ok_and_has_a_hint() {
        // a probe that knows everything EXCEPT agy
        let st = check_tools(|name| name != "agy");
        let agy = st.iter().find(|t| t.name == "agy").expect("agy reported");
        assert!(!agy.ok, "agy must be flagged missing");
        assert!(!agy.hint.is_empty(), "a missing tool must carry a hint");
        // every OTHER tool stays ok with no hint
        for t in st.iter().filter(|t| t.name != "agy") {
            assert!(t.ok, "{} should be ok", t.name);
            assert!(t.hint.is_empty(), "{} should carry no hint", t.name);
        }
    }

    /// Build a `ToolStatus` slice the way `run_checks` would, for `is_ready` tests.
    fn statuses(clis_present: &[&str], git: bool) -> Vec<ToolStatus> {
        let mut v = check_tools(|name| clis_present.contains(&name));
        v.push(ToolStatus {
            name: GIT_REPO.to_string(),
            ok: git,
            hint: String::new(),
        });
        v
    }

    #[test]
    fn ready_needs_git_repo_and_at_least_one_cli() {
        assert!(
            is_ready(&statuses(&["codex"], true)),
            "one CLI + git repo is enough to run"
        );
        assert!(
            is_ready(&statuses(&["codex", "claude", "opencode", "agy"], true)),
            "all CLIs + git repo is ready"
        );
    }

    #[test]
    fn not_ready_without_git_repo() {
        assert!(
            !is_ready(&statuses(&["codex", "claude"], false)),
            "no git repo → not ready (edits can't be bundled back)"
        );
    }

    #[test]
    fn not_ready_without_any_cli() {
        assert!(
            !is_ready(&statuses(&[], true)),
            "git repo but zero CLIs → nothing can run a turn"
        );
    }
}
