//! `ensemble mcp install --client <claude|codex|opencode>` — write each AI CLI's MCP-server config so
//! it launches `ensemble mcp` and becomes a crew member, WITHOUT the user hand-editing per-client
//! config formats.
//!
//! Design rule (for OSS portability): everything ENVIRONMENT/USER-specific is DERIVED by the caller
//! (exe = `current_exe`, repo = cwd, home from `$HOME`/`%USERPROFILE%`, plus each CLI's documented env
//! override) and never hardcoded; only each client's CONFIG FORMAT is encoded here, isolated behind one
//! renderer per client and overridable end-to-end via `--config`/`--print`. Every merge is IDEMPOTENT
//! and preserves the rest of the user's config (other MCP servers, comments).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Which AI CLI to register `ensemble mcp` into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Claude,
    Codex,
    Opencode,
}

impl ClientKind {
    /// Parse the `--client` value, naming the accepted set on error (no silent default).
    pub fn parse(s: &str) -> Result<ClientKind, String> {
        match s {
            "claude" => Ok(ClientKind::Claude),
            "codex" => Ok(ClientKind::Codex),
            "opencode" => Ok(ClientKind::Opencode),
            other => Err(format!(
                "unknown --client '{other}' (expected one of: claude | codex | opencode)"
            )),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Claude => "claude",
            ClientKind::Codex => "codex",
            ClientKind::Opencode => "opencode",
        }
    }
}

/// The DERIVED parameters baked into the generated MCP-server entry. The caller fills these from the
/// environment (never hardcoded): `exe` = the running ensemble binary, `repo` = cwd, etc.
#[derive(Debug, Clone)]
pub struct InstallParams {
    /// Path to the ensemble binary the CLI will launch (`current_exe()` by default).
    pub exe: PathBuf,
    /// `--repo` for `ensemble mcp` (absolute path to the crew session repo).
    pub repo: PathBuf,
    /// `--team` for `ensemble mcp`.
    pub team: String,
    /// `--name` — this member's identity (server-set, never client-supplied at runtime).
    pub name: String,
    /// `--crew` — `mcp_runner` tolerates a missing file (then `ensemble_run` is simply not advertised).
    pub crew: PathBuf,
}

impl InstallParams {
    /// The argv passed to the ensemble binary AFTER the exe path:
    /// `mcp --repo <repo> --team <team> --name <name> --crew <crew>`.
    pub fn server_args(&self) -> Vec<String> {
        vec![
            "mcp".to_string(),
            "--repo".to_string(),
            self.repo.to_string_lossy().into_owned(),
            "--team".to_string(),
            self.team.clone(),
            "--name".to_string(),
            self.name.clone(),
            "--crew".to_string(),
            self.crew.to_string_lossy().into_owned(),
        ]
    }
}

/// Environment inputs needed to LOCATE codex's user config — passed in so `config_path` stays pure +
/// testable (the IO shell fills these from `std::env`). claude + opencode are PROJECT-scoped (a file in
/// the repo), so they need no home/env at all — the most portable choice.
#[derive(Debug, Clone)]
pub struct Env {
    /// `%USERPROFILE%` (Windows) / `$HOME` (Unix).
    pub home: PathBuf,
    /// `$CODEX_HOME` — codex's documented config-dir override (default `<home>/.codex`).
    pub codex_home: Option<PathBuf>,
}

/// The DEFAULT path of the config we merge into for `client`, rooted at `repo`. claude + opencode use a
/// PROJECT-scoped file inside the repo (self-contained, committable, no home mutation); codex has no
/// project-scoped MCP config, so it's `$CODEX_HOME/config.toml` (default `<home>/.codex/config.toml`).
/// This is only the default — the CLI's `--config <path>` overrides it entirely.
pub fn config_path(client: ClientKind, repo: &Path, env: &Env) -> PathBuf {
    match client {
        ClientKind::Claude => repo.join(".mcp.json"),
        ClientKind::Opencode => repo.join("opencode.json"),
        ClientKind::Codex => env
            .codex_home
            .clone()
            .unwrap_or_else(|| env.home.join(".codex"))
            .join("config.toml"),
    }
}

/// The default crew-member name for `client` on a machine whose RAW host name is `raw_host` (the IO shell
/// supplies it from `$COMPUTERNAME` / `$HOSTNAME` / the `hostname` command): `<client>@<short-host>`,
/// where short-host is the FIRST dot-separated label, lowercased, with anything outside `[a-z0-9_-]`
/// dropped (a member name flows into board posts, the ledger's `claimed_by`, and the baked `--name` arg,
/// so keep it tame). Falls back to the BARE client name when no usable host is available, so a member
/// always has a name. Deterministic for a given (client, host) — the name is STABLE across restarts, which
/// ledger claim-ownership + orphan-recover rely on (so it must NOT key on a per-launch counter). Across
/// the fleet (one CLI per host) `<client>@<host>` is collision-free with zero coordination.
pub fn default_member_name(client: ClientKind, raw_host: Option<&str>) -> String {
    crate::team::default_member_name(client.as_str(), raw_host)
}

/// opencode's per-request MCP timeout defaults to 5s — far too short for a long `ensemble_run`/
/// `ensemble_merge` tool call. We set a generous one so governed sub-runs aren't killed mid-flight.
const OPENCODE_TIMEOUT_MS: u64 = 600_000;

/// Merge ensemble's MCP-server entry into `existing` config text for `client`, returning the new full
/// text. Pure + idempotent: re-running replaces ensemble's entry rather than duplicating it, and the
/// rest of `existing` is preserved. An `existing` that is present-but-malformed for the format is an
/// error (we never clobber a config we can't safely parse).
pub fn render_merged(
    client: ClientKind,
    existing: &str,
    params: &InstallParams,
) -> Result<String, String> {
    match client {
        ClientKind::Claude => render_claude(existing, params),
        ClientKind::Opencode => render_opencode(existing, params),
        ClientKind::Codex => render_codex(existing, params),
    }
}

/// Remove ensemble's MCP-server entry from an existing client config. Returns `None` when there is no
/// ensemble entry to remove, so the caller can avoid rewriting unrelated config files.
pub fn render_removed(client: ClientKind, existing: &str) -> Result<Option<String>, String> {
    match client {
        ClientKind::Claude => remove_json_object(existing, "mcpServers", "ensemble"),
        ClientKind::Opencode => remove_json_object(existing, "mcp", "ensemble"),
        ClientKind::Codex => remove_codex(existing),
    }
}

/// claude `.mcp.json`: set `mcpServers.ensemble = { command, args }`, preserving any other servers.
fn render_claude(existing: &str, params: &InstallParams) -> Result<String, String> {
    let entry = json!({
        "command": params.exe.to_string_lossy(),
        "args": params.server_args(),
    });
    merge_json_object(existing, "mcpServers", "ensemble", entry, false)
}

/// opencode `opencode.json`: set `mcp.ensemble = { type:"local", command:[exe, ...args], enabled, timeout }`.
fn render_opencode(existing: &str, params: &InstallParams) -> Result<String, String> {
    let mut command = vec![params.exe.to_string_lossy().into_owned()];
    command.extend(params.server_args());
    let entry = json!({
        "type": "local",
        "command": command,
        "enabled": true,
        "timeout": OPENCODE_TIMEOUT_MS,
    });
    merge_json_object(existing, "mcp", "ensemble", entry, true)
}

/// Insert/replace `outer.<inner>` = `entry` in a JSON config, preserving everything else. `add_schema`
/// stamps opencode's `$schema` only when creating a fresh file. A present-but-non-JSON config, or a
/// non-object `outer`, is an error (don't clobber). Output is pretty-printed with a trailing newline.
fn merge_json_object(
    existing: &str,
    outer: &str,
    inner: &str,
    entry: Value,
    add_schema: bool,
) -> Result<String, String> {
    let fresh = existing.trim().is_empty();
    let mut root: Value = if fresh {
        json!({})
    } else {
        serde_json::from_str(existing)
            .map_err(|e| format!("existing config is not valid JSON: {e}"))?
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "existing config is not a JSON object".to_string())?;
    if fresh && add_schema {
        obj.insert(
            "$schema".to_string(),
            json!("https://opencode.ai/config.json"),
        );
    }
    let servers = obj.entry(outer).or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| format!("`{outer}` in existing config is not an object"))?;
    servers.insert(inner.to_string(), entry);
    Ok(serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n")
}

fn remove_json_object(existing: &str, outer: &str, inner: &str) -> Result<Option<String>, String> {
    if existing.trim().is_empty() {
        return Ok(None);
    }
    let mut root: Value = serde_json::from_str(existing)
        .map_err(|e| format!("existing config is not valid JSON: {e}"))?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "existing config is not a JSON object".to_string())?;
    let Some(servers) = obj.get_mut(outer) else {
        return Ok(None);
    };
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| format!("`{outer}` in existing config is not an object"))?;
    if servers.remove(inner).is_none() {
        return Ok(None);
    }
    if servers.is_empty() {
        obj.remove(outer);
    }
    Ok(Some(
        serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n",
    ))
}

/// codex `config.toml`: STRUCTURALLY set `mcp_servers.ensemble.{command, args}` with `toml_edit`, which
/// preserves the rest of the document (comments, other tables, formatting) and is idempotent by
/// construction — re-installing just updates command/args in place (keeping any other fields the user
/// added under that server). There are NO text markers, so a marker-looking string in a user value can
/// never be mistaken for structure (the whole class of bug the earlier text approach suffered).
/// `toml_edit` escapes all string values correctly, so any exe path / `--name` yields valid TOML. A
/// present-but-malformed config — or an `mcp_servers` / `mcp_servers.ensemble` that exists but is NOT a
/// table — is an error rather than a clobber.
fn render_codex(existing: &str, params: &InstallParams) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = if existing.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        existing
            .parse()
            .map_err(|e| format!("existing config is not valid TOML: {e}"))?
    };
    // Get-or-create `mcp_servers` as a real table (implicit, so it emits no redundant `[mcp_servers]`
    // header when it only holds sub-tables). `as_table_mut()` is None for a non-table the user wrote
    // (a scalar / inline value) → refuse rather than clobber it.
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers` in existing config is not a table".to_string())?;
    let ensemble = servers
        .entry("ensemble")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers.ensemble` in existing config is not a table".to_string())?;
    let mut args = toml_edit::Array::new();
    for a in params.server_args() {
        args.push(a);
    }
    // set command/args (replacing any prior values), preserving any OTHER fields the user added here.
    ensemble["command"] = toml_edit::value(params.exe.to_string_lossy().into_owned());
    ensemble["args"] = toml_edit::value(args);
    Ok(doc.to_string())
}

fn remove_codex(existing: &str) -> Result<Option<String>, String> {
    if existing.trim().is_empty() {
        return Ok(None);
    }
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| format!("existing config is not valid TOML: {e}"))?;
    let Some(servers) = doc.get_mut("mcp_servers") else {
        return Ok(None);
    };
    let servers = servers
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers` in existing config is not a table".to_string())?;
    if servers.remove("ensemble").is_none() {
        return Ok(None);
    }
    if servers.is_empty() {
        doc.remove("mcp_servers");
    }
    Ok(Some(doc.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> InstallParams {
        InstallParams {
            exe: PathBuf::from(r"C:\ctgt\ensemble\debug\ensemble.exe"),
            repo: PathBuf::from(r"D:\crewdemo"),
            team: "ops".to_string(),
            name: "codex".to_string(),
            crew: PathBuf::from(r"D:\crewdemo\crew.toml"),
        }
    }

    #[test]
    fn default_member_name_appends_short_lowercased_host() {
        assert_eq!(
            default_member_name(ClientKind::Claude, Some("node-a")),
            "claude@node-a"
        );
        assert_eq!(
            default_member_name(ClientKind::Codex, Some("node-a.local")),
            "codex@node-a",
            "domain stripped"
        );
        assert_eq!(
            default_member_name(ClientKind::Opencode, Some("  node-b \n")),
            "opencode@node-b",
            "trimmed"
        );
    }

    #[test]
    fn default_member_name_falls_back_to_bare_client_without_a_usable_host() {
        assert_eq!(
            default_member_name(ClientKind::Claude, None),
            "claude",
            "no host → bare client"
        );
        assert_eq!(
            default_member_name(ClientKind::Claude, Some("")),
            "claude",
            "empty → bare"
        );
        assert_eq!(
            default_member_name(ClientKind::Claude, Some("...")),
            "claude",
            "all-stripped → bare"
        );
    }

    #[test]
    fn default_member_name_sanitizes_unexpected_chars() {
        // a name flows into board posts / ledger claimed_by / the baked --name arg, so keep it tame.
        assert_eq!(
            default_member_name(ClientKind::Codex, Some("My Box!")),
            "codex@mybox"
        );
    }

    #[test]
    fn client_parse_accepts_known_rejects_unknown() {
        assert_eq!(ClientKind::parse("claude").unwrap(), ClientKind::Claude);
        assert_eq!(ClientKind::parse("codex").unwrap(), ClientKind::Codex);
        assert_eq!(ClientKind::parse("opencode").unwrap(), ClientKind::Opencode);
        let err = ClientKind::parse("cursor").unwrap_err();
        assert!(
            err.contains("cursor") && err.contains("claude"),
            "names the bad + the valid set: {err}"
        );
    }

    #[test]
    fn config_path_is_project_scoped_for_claude_and_opencode() {
        let env = Env {
            home: PathBuf::from("/home/u"),
            codex_home: None,
        };
        let repo = Path::new("/work/repo");
        assert_eq!(
            config_path(ClientKind::Claude, repo, &env),
            repo.join(".mcp.json")
        );
        assert_eq!(
            config_path(ClientKind::Opencode, repo, &env),
            repo.join("opencode.json")
        );
    }

    #[test]
    fn config_path_codex_uses_home_then_codex_home_override() {
        let repo = Path::new("/work/repo");
        let env = Env {
            home: PathBuf::from("/home/u"),
            codex_home: None,
        };
        assert_eq!(
            config_path(ClientKind::Codex, repo, &env),
            PathBuf::from("/home/u/.codex/config.toml"),
            "default is <home>/.codex/config.toml"
        );
        let env2 = Env {
            home: PathBuf::from("/home/u"),
            codex_home: Some(PathBuf::from("/custom/codex")),
        };
        assert_eq!(
            config_path(ClientKind::Codex, repo, &env2),
            PathBuf::from("/custom/codex/config.toml"),
            "CODEX_HOME override wins over <home>/.codex"
        );
    }

    #[test]
    fn claude_render_into_empty_is_valid_and_correct() {
        let out = render_merged(ClientKind::Claude, "", &params()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let e = &v["mcpServers"]["ensemble"];
        assert_eq!(e["command"], r"C:\ctgt\ensemble\debug\ensemble.exe");
        let args: Vec<&str> = e["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "mcp",
                "--repo",
                r"D:\crewdemo",
                "--team",
                "ops",
                "--name",
                "codex",
                "--crew",
                r"D:\crewdemo\crew.toml",
            ]
        );
    }

    #[test]
    fn claude_render_preserves_other_servers() {
        let existing = r#"{ "mcpServers": { "other": { "command": "x" } }, "foo": 1 }"#;
        let out = render_merged(ClientKind::Claude, existing, &params()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["other"]["command"], "x",
            "existing server kept"
        );
        assert!(v["mcpServers"]["ensemble"].is_object(), "ensemble added");
        assert_eq!(v["foo"], 1, "unrelated keys kept");
    }

    #[test]
    fn claude_render_is_idempotent() {
        let once = render_merged(ClientKind::Claude, "", &params()).unwrap();
        let twice = render_merged(ClientKind::Claude, &once, &params()).unwrap();
        assert_eq!(once, twice, "re-installing is a no-op, never duplicates");
        let v: Value = serde_json::from_str(&twice).unwrap();
        assert_eq!(
            v["mcpServers"].as_object().unwrap().len(),
            1,
            "exactly one server"
        );
    }

    #[test]
    fn claude_remove_deletes_only_ensemble_server() {
        let existing = r#"{ "mcpServers": { "ensemble": { "command": "old" }, "other": { "command": "x" } }, "foo": 1 }"#;
        let out = render_removed(ClientKind::Claude, existing)
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcpServers"]["ensemble"].is_null());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["foo"], 1);
    }

    #[test]
    fn json_remove_is_noop_when_ensemble_entry_is_absent() {
        assert!(
            render_removed(ClientKind::Claude, r#"{ "mcpServers": { "other": {} } }"#)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn json_render_rejects_a_non_json_existing_without_clobbering() {
        let err = render_merged(ClientKind::Claude, "this is not json {", &params()).unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn opencode_render_sets_type_command_enabled_and_long_timeout() {
        let out = render_merged(ClientKind::Opencode, "", &params()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let e = &v["mcp"]["ensemble"];
        assert_eq!(e["type"], "local");
        assert_eq!(e["enabled"], true);
        assert_eq!(
            e["timeout"], 600_000,
            "long timeout so ensemble_run isn't killed at opencode's 5s default"
        );
        let cmd: Vec<&str> = e["command"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(
            cmd[0], r"C:\ctgt\ensemble\debug\ensemble.exe",
            "exe is command[0]"
        );
        assert_eq!(
            &cmd[1..],
            [
                "mcp",
                "--repo",
                r"D:\crewdemo",
                "--team",
                "ops",
                "--name",
                "codex",
                "--crew",
                r"D:\crewdemo\crew.toml",
            ]
        );
    }

    #[test]
    fn opencode_render_preserves_other_servers() {
        let existing = r#"{ "mcp": { "pw": { "type": "local", "command": ["x"] } } }"#;
        let out = render_merged(ClientKind::Opencode, existing, &params()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcp"]["pw"].is_object(), "existing mcp server kept");
        assert!(v["mcp"]["ensemble"].is_object(), "ensemble added");
    }

    #[test]
    fn opencode_remove_deletes_mcp_outer_when_empty() {
        let existing = r#"{ "$schema": "https://opencode.ai/config.json", "mcp": { "ensemble": { "type": "local" } } }"#;
        let out = render_removed(ClientKind::Opencode, existing)
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcp"].is_null(), "empty mcp object removed");
        assert_eq!(v["$schema"], "https://opencode.ai/config.json");
    }

    #[test]
    fn codex_render_into_empty_creates_a_valid_table() {
        let out = render_merged(ClientKind::Codex, "", &params()).unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        let srv = &v["mcp_servers"]["ensemble"];
        assert_eq!(
            srv["command"].as_str().unwrap(),
            r"C:\ctgt\ensemble\debug\ensemble.exe"
        );
        let args: Vec<&str> = srv["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "mcp",
                "--repo",
                r"D:\crewdemo",
                "--team",
                "ops",
                "--name",
                "codex",
                "--crew",
                r"D:\crewdemo\crew.toml",
            ]
        );
    }

    #[test]
    fn codex_render_preserves_other_tables_comments_and_is_idempotent() {
        let existing = "# my codex config\nmodel = \"gpt-5\"\n\n[plugins.foo]\nenabled = true\n";
        let once = render_merged(ClientKind::Codex, existing, &params()).unwrap();
        // other content (incl. the comment + the other table) survives — toml_edit is format-preserving
        assert!(once.contains("# my codex config"), "comment kept");
        assert!(once.contains("model = \"gpt-5\""));
        assert!(once.contains("[plugins.foo]"));
        let v: toml::Value = toml::from_str(&once).unwrap();
        assert_eq!(
            v["mcp_servers"]["ensemble"]["command"].as_str().unwrap(),
            r"C:\ctgt\ensemble\debug\ensemble.exe"
        );
        // idempotent: re-installing identical params is a byte-for-byte no-op, exactly one table
        let twice = render_merged(ClientKind::Codex, &once, &params()).unwrap();
        assert_eq!(once, twice, "re-install is a no-op");
        assert_eq!(
            twice.matches("[mcp_servers.ensemble]").count(),
            1,
            "exactly one table"
        );
    }

    #[test]
    fn codex_render_updates_in_place_and_keeps_extra_user_fields() {
        // a prior server entry with an EXTRA user field: re-install with a new name updates command/args
        // in place but PRESERVES the extra field and adds no second table.
        let existing =
            "[mcp_servers.ensemble]\ncommand = 'old'\nargs = []\nstartup_timeout_ms = 20000\n";
        let mut p2 = params();
        p2.name = "codex-2".to_string();
        let out = render_merged(ClientKind::Codex, existing, &p2).unwrap();
        assert_eq!(out.matches("[mcp_servers.ensemble]").count(), 1);
        let v: toml::Value = toml::from_str(&out).unwrap();
        let srv = &v["mcp_servers"]["ensemble"];
        assert_eq!(
            srv["command"].as_str().unwrap(),
            r"C:\ctgt\ensemble\debug\ensemble.exe",
            "command updated"
        );
        let args: Vec<&str> = srv["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(
            args.contains(&"codex-2"),
            "args updated to the new name: {args:?}"
        );
        assert_eq!(
            srv["startup_timeout_ms"].as_integer().unwrap(),
            20000,
            "the user's extra field is preserved"
        );
    }

    #[test]
    fn codex_remove_deletes_ensemble_table_and_keeps_other_content() {
        let existing = "# config\nmodel = \"gpt-5\"\n\n[mcp_servers.ensemble]\ncommand = 'old'\nargs = []\n\n[plugins.foo]\nenabled = true\n";
        let out = render_removed(ClientKind::Codex, existing)
            .unwrap()
            .unwrap();
        assert!(out.contains("# config"));
        assert!(out.contains("model = \"gpt-5\""));
        assert!(out.contains("[plugins.foo]"));
        assert!(!out.contains("[mcp_servers.ensemble]"));
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert!(v.get("mcp_servers").is_none());
    }

    #[test]
    fn codex_remove_is_noop_without_ensemble_entry() {
        let existing = "model = \"gpt-5\"\n[mcp_servers.other]\ncommand = 'x'\n";
        assert!(render_removed(ClientKind::Codex, existing)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_render_rejects_a_malformed_existing_config_without_clobbering() {
        let err = render_merged(ClientKind::Codex, "this = = not toml", &params()).unwrap_err();
        assert!(err.contains("not valid TOML"), "got: {err}");
    }

    #[test]
    fn codex_render_refuses_a_non_table_mcp_servers() {
        // a scalar `mcp_servers` (or `mcp_servers.ensemble`) must NOT be indexed-through and clobbered.
        let err =
            render_merged(ClientKind::Codex, "mcp_servers = \"oops\"\n", &params()).unwrap_err();
        assert!(err.contains("not a table"), "got: {err}");
        let err2 = render_merged(
            ClientKind::Codex,
            "[mcp_servers]\nensemble = 1\n",
            &params(),
        )
        .unwrap_err();
        assert!(err2.contains("not a table"), "got: {err2}");
    }

    #[test]
    fn codex_render_yields_valid_toml_for_a_weird_name() {
        // toml_edit escapes any string — a pathological --name (quote + newline) round-trips as valid TOML.
        let mut p = params();
        p.name = "a\"b\nc".to_string();
        let out = render_merged(ClientKind::Codex, "", &p).unwrap();
        let v: toml::Value = toml::from_str(&out).expect("valid TOML");
        let args: Vec<&str> = v["mcp_servers"]["ensemble"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(
            args.contains(&"a\"b\nc"),
            "the weird name round-trips: {args:?}"
        );
    }
}
