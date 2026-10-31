//! `ensemble doctor` — environment-readiness check. Reports, before a crew runs, whether each
//! thing the mesh needs is present: the AI CLIs (codex/claude/opencode/agy/spectyn), `tailscale` (for
//! cross-machine discovery), and whether the cwd is a git repo. The CORE (`check_tools` / `is_ready`)
//! is pure — it asks a `present(name)` probe / reads a `&[ToolStatus]` — so it is hermetically
//! testable; `run_checks` is the thin IO wrapper that supplies a real PATH probe + a git-repo check.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    // the date must end there: `)`, whitespace or the end of the line, not `2026-10-30123`
    let ends = match rest[10..].chars().next() {
        None => true,
        Some(c) => c == ')' || c.is_whitespace(),
    };
    (shape_ok && ends).then(|| d.to_string())
}

/// Every `spectyn` on PATH in resolution order, one per directory (a directory holding both a
/// `spectyn.exe` and a `spectyn.cmd` shim is one installation). PATH is walked here rather than in
/// a shell, so an entry with glob or space characters is taken literally. On Windows the cwd is
/// searched first and paths compare case-insensitively, as cmd (which ensemble launches spectyn
/// through) does; elsewhere paths are case-sensitive.
fn spectyn_paths() -> Vec<String> {
    let windows = cfg!(windows);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if windows {
        dirs.extend(std::env::current_dir().ok());
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let names = exe_names("spectyn", std::env::var("PATHEXT").ok().as_deref(), windows);
    find_on_path(&dirs, &names, windows, is_executable_file)
}

/// PURE: the file names a bare `name` resolves to inside one directory, most preferred first. On
/// Windows that is `name.exe`, then `name` plus each other `PATHEXT` extension (default
/// `.COM;.EXE;.BAT;.CMD`); elsewhere just `name`.
fn exe_names(name: &str, pathext: Option<&str>, windows: bool) -> Vec<String> {
    if !windows {
        return vec![name.to_string()];
    }
    let mut out = vec![format!("{name}.exe")];
    for ext in pathext.unwrap_or(".COM;.EXE;.BAT;.CMD").split(';') {
        let ext = ext.trim().to_ascii_lowercase();
        let file = format!("{name}{ext}");
        if ext.len() > 1 && ext.starts_with('.') && !out.contains(&file) {
            out.push(file);
        }
    }
    out
}

/// PURE: walk `dirs` in order and keep, per directory, the first of `names` that `is_exe` accepts,
/// so several shims in one directory count as one copy. Empty entries are skipped and a path seen
/// before (a directory listed twice) is kept once; `fold_case` compares paths case-insensitively,
/// which is right only on Windows.
fn find_on_path(
    dirs: &[PathBuf],
    names: &[String],
    fold_case: bool,
    is_exe: impl Fn(&Path) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in dirs.iter().filter(|d| !d.as_os_str().is_empty()) {
        let Some(hit) = names.iter().map(|n| dir.join(n)).find(|p| is_exe(p)) else {
            continue;
        };
        let hit = hit.to_string_lossy().into_owned();
        let seen = out.iter().any(|p| {
            if fold_case {
                p.eq_ignore_ascii_case(&hit)
            } else {
                *p == hit
            }
        });
        if !seen {
            out.push(hit);
        }
    }
    out
}

/// A regular file that can be run: on Unix it must also have an execute bit set.
fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Run `<path> --version` and return its first non-empty stdout line (see `first_line_within`).
fn version_of(path: &str, timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    first_line_within(cmd, timeout)
}

/// `cmd`'s first non-empty stdout line, or `None` on a spawn error, a non-zero exit, no output, or
/// when it has not both closed its output and exited by the deadline. ONE deadline (`timeout` from
/// now) covers the read and the exit, so a child that closes stdout but keeps running is killed
/// too; stdout is read on a helper thread so a stuck pipe cannot stall doctor either.
fn first_line_within(mut cmd: Command, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let mut child = cmd
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
    let text = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok();
    let status = text.as_ref().and_then(|_| exit_by(&mut child, deadline));
    let (Some(text), Some(status)) = (text, status) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !status.success() {
        return None;
    }
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Poll `child` until it exits (`Some(status)`), or `None` once `deadline` passes or waiting fails.
fn exit_by(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => return None,
        }
    }
}

/// The AI CLIs actually on PATH (the names a crew can drive locally). Used by `ensemble mesh`.
pub fn present_clis() -> Vec<String> {
    check_tools(on_path)
        .into_iter()
        .filter(|t| t.ok)
        .map(|t| t.name)
        .collect()
}

/// True if the program an adapter would exec is installed here: a value with a directory part (an
/// `ENSEMBLE_<X>_BIN`-style override such as `C:/bin/spectyn.exe`) must name an existing executable
/// file; a bare name must resolve on PATH. Used by `ensemble serve` to advertise only runnable vendors.
pub fn program_present(program: &str) -> bool {
    let p = Path::new(program);
    if p.components().count() > 1 {
        is_executable_file(p)
    } else {
        on_path(program)
    }
}

/// A path override must be a regular file the OS can exec. On Unix that also needs an execute bit —
/// otherwise the spawn fails with permission denied; on Windows `cmd /C` runs any existing file.
fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
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
        // the date must be delimited: `)`, whitespace or end of line
        assert_eq!(build_date("built 2026-10-30123"), None);
        assert_eq!(build_date("built 2026-10-30x"), None);
        assert_eq!(
            build_date("built 2026-10-30").as_deref(),
            Some("2026-10-30")
        );
        assert_eq!(
            build_date("spectyn 1 built 2026-10-30 extra").as_deref(),
            Some("2026-10-30")
        );
    }

    /// `dir` joined with `name` as the PATH walk renders it (separator differs per OS).
    fn joined(dir: &str, name: &str) -> String {
        Path::new(dir).join(name).to_string_lossy().into_owned()
    }

    fn dirs(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn exe_names_prefer_exe_then_pathext_on_windows_only() {
        assert_eq!(
            exe_names("spectyn", Some(".EXE;.CMD"), false),
            vec!["spectyn"]
        );
        assert_eq!(
            exe_names("spectyn", Some(".COM;.EXE;.BAT;.CMD"), true),
            vec!["spectyn.exe", "spectyn.com", "spectyn.bat", "spectyn.cmd"]
        );
        // PATHEXT unset: the cmd default; junk entries are ignored
        assert_eq!(exe_names("spectyn", None, true).len(), 4);
        assert_eq!(
            exe_names("spectyn", Some(";.;CMD;.Cmd"), true),
            vec!["spectyn.exe", "spectyn.cmd"]
        );
    }

    #[test]
    fn path_walk_counts_shims_in_one_directory_as_one_copy() {
        // npm-style shims: spectyn.cmd + spectyn.exe side by side are one installation (the .exe
        // is reported), so no false "2 copies" warning; a later dir with only a .cmd still counts
        let files = [
            joined("C:/shims", "spectyn.cmd"),
            joined("C:/shims", "spectyn.exe"),
            joined("C:/cli", "spectyn.cmd"),
        ];
        let names = exe_names("spectyn", Some(".COM;.EXE;.BAT;.CMD"), true);
        let got = find_on_path(
            &dirs(&["C:/shims", "C:/empty", "C:/cli"]),
            &names,
            true,
            |p: &Path| files.contains(&p.to_string_lossy().into_owned()),
        );
        assert_eq!(
            got,
            vec![
                joined("C:/shims", "spectyn.exe"),
                joined("C:/cli", "spectyn.cmd")
            ]
        );
    }

    #[test]
    fn path_walk_folds_case_only_when_asked() {
        // Unix paths are case-sensitive: /opt/A and /opt/a are two copies; Windows folds them.
        // Empty PATH entries are skipped and a repeated directory is reported once.
        let path = dirs(&["/opt/A", "", "/opt/a", "/opt/A"]);
        let names = exe_names("spectyn", None, false);
        let any = |_: &Path| true;
        assert_eq!(
            find_on_path(&path, &names, false, any),
            vec![joined("/opt/A", "spectyn"), joined("/opt/a", "spectyn")]
        );
        assert_eq!(
            find_on_path(&path, &names, true, any),
            vec![joined("/opt/A", "spectyn")]
        );
    }

    #[test]
    fn path_walk_takes_directory_names_literally_and_needs_an_executable() {
        // `d[x] y` would be a glob matching `dx y` in a shell; the walk uses the name as-is
        let tmp = tempfile::tempdir().unwrap();
        let odd = tmp.path().join("d[x] y");
        let lookalike = tmp.path().join("dx y");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::create_dir_all(&lookalike).unwrap();
        let names = exe_names("spectyn", None, cfg!(windows));
        let exe = odd.join(&names[0]);
        std::fs::write(&exe, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            // a file without an execute bit is not a runnable copy
            std::fs::write(lookalike.join("spectyn"), b"").unwrap();
        }
        let got = find_on_path(&[odd, lookalike], &names, cfg!(windows), is_executable_file);
        assert_eq!(got, vec![exe.to_string_lossy().into_owned()]);
    }

    /// A command that prints `spectyn 9` and exits 0 at once.
    fn quick() -> Command {
        let mut c = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            c.args(["/C", "echo", "spectyn", "9"]);
        } else {
            c.args(["-c", "echo spectyn 9"]);
        }
        c
    }

    /// A command that keeps running ~30 s. On Unix it prints a line and then CLOSES stdout, so
    /// reading ends at once and only the exit wait could stall (the round-1 bug); on Windows
    /// `ping` keeps writing, so the read is what times out.
    fn lingering() -> Command {
        let mut c = Command::new(if cfg!(windows) { "ping" } else { "sh" });
        if cfg!(windows) {
            c.args(["-n", "30", "127.0.0.1"]);
        } else {
            c.args(["-c", "echo spectyn 9; exec sleep 30 >&-"]);
        }
        c
    }

    #[test]
    fn first_line_within_returns_a_quick_commands_line() {
        assert_eq!(
            first_line_within(quick(), Duration::from_secs(20)).as_deref(),
            Some("spectyn 9")
        );
    }

    #[test]
    fn first_line_within_kills_a_command_that_outlives_the_deadline() {
        let start = Instant::now();
        assert_eq!(
            first_line_within(lingering(), Duration::from_millis(500)),
            None
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "one deadline must bound read + exit; took {:?}",
            start.elapsed()
        );
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
    fn program_present_checks_a_path_override_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("spectyn.exe");
        std::fs::write(&bin, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(
            program_present(bin.to_str().unwrap()),
            "existing override file"
        );
        let missing = dir.path().join("no-such-spectyn.exe");
        assert!(
            !program_present(missing.to_str().unwrap()),
            "missing override file"
        );
        assert!(
            !program_present(dir.path().to_str().unwrap()),
            "a directory is not a program"
        );
    }

    #[cfg(unix)]
    #[test]
    fn program_present_needs_an_execute_bit_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("spectyn");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            !program_present(bin.to_str().unwrap()),
            "non-executable file → permission denied"
        );
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            program_present(bin.to_str().unwrap()),
            "chmod +x → runnable"
        );
    }

    #[test]
    fn program_present_is_false_for_a_bare_name_not_on_path() {
        assert!(!program_present("ensemble-wp140-no-such-cli"));
    }

    #[test]
    fn not_ready_without_any_cli() {
        assert!(
            !is_ready(&statuses(&[], true)),
            "git repo but zero CLIs → nothing can run a turn"
        );
    }
}
