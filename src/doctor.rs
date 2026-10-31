//! `ensemble doctor` — environment-readiness check. Reports, before a crew runs, whether each
//! thing the mesh needs is present: the AI CLIs (codex/claude/opencode/agy/spectyn), `tailscale` (for
//! cross-machine discovery), and whether the cwd is a git repo. The CORE (`check_tools` / `is_ready`)
//! is pure — it asks a `present(name)` probe / reads a `&[ToolStatus]` — so it is hermetically
//! testable; `run_checks` is the thin IO wrapper that supplies a real PATH probe + a git-repo check.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// One readiness item: a named thing the mesh wants, whether it is present, and (when missing) a
/// one-line hint telling the operator how to satisfy it. A present item may still carry a detail in
/// `hint` (spectyn's version line) and be flagged `warn`; a warning never affects readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatus {
    pub name: String,
    pub ok: bool,
    pub warn: bool,
    pub hint: String,
}

/// The AI CLIs the crew drives, paired with the hint shown when one is missing. Order is the order
/// the report prints them in.
const TOOLS: &[(&str, &str)] = &[
    ("codex", "install + log in: codex"),
    ("claude", "install + log in: claude (Claude Code)"),
    ("opencode", "install + log in: opencode"),
    ("agy", "install + log in: agy (antigravity)"),
    (
        "spectyn",
        "optional: install spectyn (headless `spectyn exec`; ENSEMBLE_SPECTYN_BIN pins the binary)",
    ),
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
                warn: false,
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
    if let Some(sp) = out.iter_mut().find(|t| t.name == "spectyn" && t.ok) {
        if let Some((warn, detail)) =
            inspect_spectyn(spectyn_paths, |p| version_of(p, VERSION_TIMEOUT))
        {
            sp.warn = warn;
            sp.hint = detail;
        }
    }
    let ts = on_path("tailscale");
    out.push(ToolStatus {
        name: "tailscale".to_string(),
        ok: ts,
        warn: false,
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
        warn: false,
        hint: if git {
            String::new()
        } else {
            "run inside a git repo so a crew's edits can be bundled back".to_string()
        },
    });
    out
}

/// How long `spectyn --version` may take before doctor gives up on it.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// PURE core of the spectyn detail. `lookup()` lists every `spectyn` on PATH in resolution order
/// (first = the one a bare `spectyn` runs); `version_of(path)` returns that copy's
/// `spectyn --version` line. `None` when no copy is found, else `(warn, detail)`: the detail leads
/// with the first copy's version line, and `warn` is set when that version could not be read, or
/// when several copies resolve and the first is not the newest by its `built YYYY-MM-DD` date (on
/// Windows an old App copy can shadow the CLI). Copies after the first are only run when there
/// are several.
pub fn inspect_spectyn(
    lookup: impl Fn() -> Vec<String>,
    version_of: impl Fn(&str) -> Option<String>,
) -> Option<(bool, String)> {
    let paths = lookup();
    let first = paths.first()?;
    let Some(first_ver) = version_of(first) else {
        return Some((
            true,
            format!("WARNING: `{first} --version` failed or timed out"),
        ));
    };
    if paths.len() < 2 {
        return Some((false, first_ver));
    }
    let first_date = build_date(&first_ver);
    let newest_other = paths[1..]
        .iter()
        .filter_map(|p| version_of(p).and_then(|v| build_date(&v)).map(|d| (d, p)))
        .max_by(|a, b| a.0.cmp(&b.0));
    let warning = match (first_date, newest_other) {
        (Some(fd), Some((od, op))) if od > fd => Some(format!(
            "first on PATH ({first}, built {fd}) is older than {op} (built {od})"
        )),
        (None, _) => Some(format!(
            "{} copies on PATH and the first ({first}) has no readable build date",
            paths.len()
        )),
        _ => None,
    };
    Some(match warning {
        Some(w) => (
            true,
            format!("{first_ver}; WARNING: {w}; fix PATH or pin one with ENSEMBLE_SPECTYN_BIN"),
        ),
        None => (false, first_ver),
    })
}

/// PURE: the `YYYY-MM-DD` after `built ` in a `spectyn 0.6.0 (<sha>, <os>, built YYYY-MM-DD)`
/// version line. ISO dates compare correctly as strings, so callers can `max` them directly.
fn build_date(version: &str) -> Option<String> {
    let rest = &version[version.find("built ")? + "built ".len()..];
    let d = rest.get(..10)?;
    let shape_ok = d.char_indices().all(|(i, c)| match i {
        4 | 7 => c == '-',
        _ => c.is_ascii_digit(),
    });
    shape_ok.then(|| d.to_string())
}

/// Every `spectyn` on PATH in resolution order, de-duplicated. `where` on Windows lists them all
/// (cwd first, as cmd resolves); on Unix walk `$PATH`, since `command -v` only gives the first.
fn spectyn_paths() -> Vec<String> {
    let (prog, args) = if cfg!(windows) {
        ("cmd", vec!["/C", "where", "spectyn"])
    } else {
        (
            "sh",
            vec![
                "-c",
                r#"IFS=:; for d in $PATH; do [ -f "$d/$1" ] && [ -x "$d/$1" ] && echo "$d/$1"; done"#,
                "sh",
                "spectyn",
            ],
        )
    };
    let Ok(o) = Command::new(prog).args(args).output() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let line = line.trim();
        if !line.is_empty() && !out.iter().any(|p| p.eq_ignore_ascii_case(line)) {
            out.push(line.to_string());
        }
    }
    out
}

/// Run `<path> --version` and return its first non-empty stdout line, or `None` on a spawn error,
/// a non-zero exit or no output. Killed after `timeout` so a hung binary cannot stall doctor; stdout
/// is read on a helper thread so a stuck pipe cannot either.
fn version_of(path: &str, timeout: Duration) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let Ok(text) = rx.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !child.wait().ok()?.success() {
        return None;
    }
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// The AI CLIs actually on PATH (the names a crew can drive locally). Used by `ensemble mesh`.
pub fn present_clis() -> Vec<String> {
    check_tools(on_path)
        .into_iter()
        .filter(|t| t.ok)
        .map(|t| t.name)
        .collect()
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
        // the expected CLIs are reported, in order
        let names: Vec<&str> = st.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["codex", "claude", "opencode", "agy", "spectyn"]);
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
            warn: false,
            hint: String::new(),
        });
        v
    }

    const NEW: &str = "spectyn 0.6.0 (abc1234, windows, built 2026-10-30)";
    const OLD: &str = "spectyn 0.6.0 (0ld0000, windows, built 2026-09-01)";

    /// Fake `version_of`: maps each fake path to a canned `--version` line (None = it failed).
    fn versions<'a>(
        table: &'a [(&'a str, Option<&'a str>)],
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |p| {
            table
                .iter()
                .find(|(path, _)| *path == p)
                .and_then(|(_, v)| v.map(str::to_string))
        }
    }

    #[test]
    fn spectyn_not_found_gives_no_detail() {
        assert_eq!(inspect_spectyn(Vec::new, versions(&[])), None);
    }

    #[test]
    fn spectyn_one_copy_shows_version_no_warning() {
        let got = inspect_spectyn(
            || vec!["C:/bin/spectyn.exe".into()],
            versions(&[("C:/bin/spectyn.exe", Some(NEW))]),
        );
        assert_eq!(got, Some((false, NEW.to_string())));
    }

    #[test]
    fn spectyn_two_copies_newest_first_no_warning() {
        let got = inspect_spectyn(
            || vec!["C:/cli/spectyn.exe".into(), "C:/app/spectyn.exe".into()],
            versions(&[
                ("C:/cli/spectyn.exe", Some(NEW)),
                ("C:/app/spectyn.exe", Some(OLD)),
            ]),
        );
        assert_eq!(got, Some((false, NEW.to_string())));
    }

    #[test]
    fn spectyn_two_copies_old_first_warns_with_both_paths() {
        let (warn, detail) = inspect_spectyn(
            || vec!["C:/app/spectyn.exe".into(), "C:/cli/spectyn.exe".into()],
            versions(&[
                ("C:/app/spectyn.exe", Some(OLD)),
                ("C:/cli/spectyn.exe", Some(NEW)),
            ]),
        )
        .expect("found");
        assert!(warn, "an older copy shadowing a newer one must warn");
        assert!(
            detail.starts_with(OLD),
            "detail leads with the version that runs: {detail}"
        );
        assert!(detail.contains("C:/app/spectyn.exe") && detail.contains("C:/cli/spectyn.exe"));
        assert!(detail.contains("2026-09-01") && detail.contains("2026-10-30"));
    }

    #[test]
    fn spectyn_unparsable_version() {
        // one copy, odd version text: shown as-is, no warning
        let got = inspect_spectyn(
            || vec!["/usr/bin/spectyn".into()],
            versions(&[("/usr/bin/spectyn", Some("spectyn dev"))]),
        );
        assert_eq!(got, Some((false, "spectyn dev".to_string())));
        // two copies, first has no build date: cannot confirm it is the newest -> warn
        let (warn, detail) = inspect_spectyn(
            || vec!["/a/spectyn".into(), "/b/spectyn".into()],
            versions(&[
                ("/a/spectyn", Some("spectyn dev")),
                ("/b/spectyn", Some(NEW)),
            ]),
        )
        .expect("found");
        assert!(warn && detail.starts_with("spectyn dev"), "{detail}");
        // two copies, a later one unparsable: ignored, the first stays the newest known
        let got = inspect_spectyn(
            || vec!["/a/spectyn".into(), "/b/spectyn".into()],
            versions(&[
                ("/a/spectyn", Some(OLD)),
                ("/b/spectyn", Some("garbage built 20x6-1")),
            ]),
        );
        assert_eq!(got, Some((false, OLD.to_string())));
        // --version failed outright: still found, but warned
        let (warn, _) = inspect_spectyn(
            || vec!["/a/spectyn".into()],
            versions(&[("/a/spectyn", None)]),
        )
        .expect("found");
        assert!(warn);
    }

    #[test]
    fn spectyn_warning_does_not_change_readiness() {
        let mut st = statuses(&["spectyn"], true);
        st.iter_mut().find(|t| t.name == "spectyn").unwrap().warn = true;
        assert!(is_ready(&st), "a spectyn warning is not a failure");
    }

    #[test]
    fn build_date_parses_only_the_iso_shape() {
        assert_eq!(build_date(NEW).as_deref(), Some("2026-10-30"));
        assert_eq!(build_date("spectyn 0.6.0"), None);
        assert_eq!(build_date("built 2026/10/30"), None);
        assert_eq!(build_date("built 2026-10"), None);
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
