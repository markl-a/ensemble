use ensemble::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Process-wide abort flag (firewall B). A single Ctrl-C handler flips it; every Conductor reads it
/// via `.with_abort(...)` and bails cleanly at the next round boundary.
static ABORT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
fn abort_flag() -> Arc<AtomicBool> {
    ABORT
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

const DEFAULT_SERVE_PORT: u16 = 7878;

const USAGE: &str = "usage:\n  \
    ensemble run \"<task>\" [--crew <crew.toml>] [--repo <path>] [--team <name>] [--merge [--into <target>]] [--watch <name>]\n  \
    ensemble run-many \"<task1>\" \"<task2>\" ... [--crew <crew.toml>] [--repo <path>]\n  \
    ensemble crew inspect [--crew <crew.toml>] [--json]   (print parsed crew/gate/reviewer metadata for verification)\n  \
    ensemble dispatch \"<task1>\" ... --ledger <db> [--crew <crew.toml>] [--repo <path>]   (durable, resumable)\n  \
    ensemble ledger <status|recover> --ledger <db> [--stale-secs N]\n  \
    ensemble nodes [--port <n>]   (probe the tailnet for `serve` hosts and the agents they offer)\n  \
    ensemble mesh [--port <n>]   (this node's CLIs + which agents each tailnet peer hosts)\n  \
    ensemble doctor   (check this machine is ready: which AI CLIs + tailscale are on PATH, is-git-repo)\n  \
    ensemble agent <name> \"<task>\" [--node auto|<host>] [--repo <path>] [--json]   (delegate ONE turn to one CLI)\n  \
    ensemble [--repo <path>] [--team <name>] [--member <member>] [--confirm-policy ask|approve|deny] [--print-config] <codex|claude|opencode> [vendor args...]\n  \
    ensemble [--repo <path>] [--team <name>] [--member <member>] [--timeout <secs>] [--confirm-policy ask|approve|deny] [--print-prompt] [--json] agy [vendor args...]   (no prompt: interactive; --prompt/-p: bounded team turn)\n  \
    ensemble merge <branch> [--into <target>] [--repo <path>] [--resolver <agent>]   (land a kept branch; conflict → escalate, or --resolver runs ONE AI round)\n  \
    ensemble serve [--bind <addr>|--port <n>] [--token <token>]   (default: this node's tailnet IP:7878; loopback if no tailnet)\n  \
    ensemble serve --install-service|--uninstall-service [--bind <addr>|--port <n>] [--token <token>] [--exe <path>] [--print]   (install/remove boot/login-started serve)\n  \
    ensemble up [--bind <addr>|--port <n>] [--token <token>]   (quick start: show the mesh, then serve in the foreground)\n  \
    ensemble mcp [--repo <path>] [--team <name>] [--name <agent>] [--crew <crew.toml>]   (stdio MCP server: make a LIVE CLI a crew member — mesh + board + queue + worktree + merge + run)\n  \
    ensemble mcp install --client <claude|codex|opencode> [--repo <p>] [--team <name>] [--name <id>] [--exe <p>] [--crew <p>] [--config <p>] [--print]   (one-click: register `ensemble mcp` into that CLI's config)\n  \
    ensemble mcp uninstall --client <claude|codex|opencode> [--repo <p>] [--config <p>] [--print]   (remove ensemble's MCP entry from that CLI's config)\n  \
  ensemble team <status|say|inbox> [--repo <path>] [--team <name>] [--node <host|url>] [--port <n>] [--token <token>] [--json]   (inspect and post to the team board)\n  \
  ensemble watch <member[@node]> [--repo <path>] [--node <host|url>] [--port <n>] [--team <name>] [--token <token>] [--since <n>] [--follow] [--json]   (tail a live member's stream feed)\n  \
    ensemble supervise <name> [--repo <path>] [--team <name>] [--agent claude] [--since <n>] [--json] [--apply-steer] [--abort-on-critical]   (ask an AI to inspect recent team/run evidence)\n  \
  ensemble steer <name[@node]> \"<prompt>\" [--repo <path>] [--node <host|url>] [--port <n>] [--token <token>]   (inject a redirect into a live --watch run's next round)\n  \
  ensemble abort <name[@node]> [--hard] [--repo <path>] [--node <host|url>] [--port <n>] [--token <token>]   (stop a live --watch run; --hard kills the running CLI now)\n  \
    ensemble all \"<prompt>\" [--repo <path>] [--no-discover] [--json]   (COUNCIL: fan one prompt to EVERY reachable CLI, side-by-side replies)\n\n\
    run/run-many/dispatch auto-discover tailnet `serve` hosts for any agent without an explicit\n  \
    [agents.<n>] node = ... in crew.toml; pass --no-discover to stay local.";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match parse_launcher_invocation(&args) {
        Ok(Some(LauncherInvocation::Member {
            client,
            args: parsed,
        })) => return member_launcher_cmd(client, parsed),
        Ok(Some(LauncherInvocation::Agy(parsed))) => return agy_cmd(parsed),
        Ok(None) => {}
        Err(e) => {
            eprintln!("ensemble: {e}");
            std::process::exit(2);
        }
    }
    let sub = args.get(1).map(|s| s.as_str());
    // Firewall B: only the conductor-driven commands honor the abort flag, so install the Ctrl-C
    // handler ONLY for them. Installing it for `serve`/`ledger`/`nodes` would swallow their default
    // Ctrl-C termination (they never read the flag), leaving them un-interruptible.
    if matches!(sub, Some("run") | Some("run-many") | Some("dispatch")) {
        let flag = abort_flag();
        let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed));
    }
    match sub {
        Some("run") => run_single(&args),
        Some("run-many") => run_many(&args),
        Some("crew") => crew_cmd(&args),
        Some("dispatch") => dispatch_cmd(&args),
        Some("ledger") => ledger_cmd(&args),
        Some("nodes") => nodes_cmd(&args),
        Some("mesh") => mesh_cmd(&args),
        Some("doctor") => doctor_cmd(&args),
        Some("agent") => agent_cmd(&args),
        Some("codex") | Some("claude") | Some("opencode") | Some("agy") => {
            eprintln!(
                "ensemble: launcher syntax is `ensemble [ensemble options] {} [vendor args...]`",
                sub.unwrap_or("<ai-cli>")
            );
            std::process::exit(2);
        }
        Some("merge") => merge_cmd(&args),
        Some("mcp") => mcp_cmd(&args),
        Some("team") => team_cmd(&args),
        Some("serve") => serve_cmd(&args),
        Some("up") => up_cmd(&args),
        Some("watch") => watch_cmd(&args),
        Some("supervise") => supervise_cmd(&args),
        Some("steer") => steer_cmd(&args),
        Some("abort") => abort_cmd(&args),
        Some("all") => all_cmd(&args),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// `ensemble serve [--bind <addr>]` — run a tiny HTTP agent-host exposing this node's local CLIs
/// (the 4-adapter registry) over `/health` + `/run`. A `RemoteAdapter` on another machine drives
/// them. Plain HTTP over the tailnet (WireGuard encrypts). Blocks forever.
fn serve_cmd(args: &[String]) {
    if has_flag(args, "--install-service") {
        return serve_install_service_cmd(args);
    }
    if has_flag(args, "--uninstall-service") {
        return serve_uninstall_service_cmd(args);
    }
    require_value_if_present(args, "--token");
    require_value_if_present(args, "--port");
    if let Err(e) = reject_bind_and_port(args) {
        eprintln!("ensemble serve: {e}");
        std::process::exit(2);
    }
    let explicit = parse_flag(args, "--bind");
    let port = parse_discovery_port_or_exit(args);
    let token = control_token(args);
    // Default to the tailnet interface so serve is reachable only over the tailnet, not the LAN.
    let self_ips = ensemble::discovery::self_tailscale_ips();
    let bind = ensemble::resolve_bind(&self_ips, explicit.as_deref(), port);
    if let ensemble::BindAddr::Loopback(_) = bind {
        eprintln!(
            "ensemble: no tailnet IP found (is tailscale up?) — binding loopback only (local). \
             Pass --bind <addr> to override."
        );
    }
    let addr = bind.addr().to_string();
    println!("ensemble serve on {addr}");
    if let Err(e) = ensemble::serve_with_token(&addr, adapters(), token) {
        eprintln!("serve: {e}");
        std::process::exit(1);
    }
}

/// `ensemble mesh` — print which AI CLIs are on THIS node + which agents each discovered tailnet
/// peer hosts. Read-only (no side effects).
fn mesh_cmd(args: &[String]) {
    require_value_if_present(args, "--port");
    let port = parse_discovery_port_or_exit(args);
    let local = ensemble::present_clis();
    let hosts = ensemble::discover_mesh(port);
    println!("{}", ensemble::render_mesh(&local, &hosts));
}

/// `ensemble up [--bind <addr>]` — the quick-start: resolve the bind (tailnet-only by default),
/// print the mesh (local CLIs + tailnet hosts), then serve in the FOREGROUND until Ctrl-C. The
/// permanent/boot-started path is `serve --install-service` (tick C), not `up`.
fn up_cmd(args: &[String]) {
    require_value_if_present(args, "--token");
    require_value_if_present(args, "--port");
    if let Err(e) = reject_bind_and_port(args) {
        eprintln!("ensemble up: {e}");
        std::process::exit(2);
    }
    let explicit = parse_flag(args, "--bind");
    let port = parse_discovery_port_or_exit(args);
    let token = control_token(args);
    let self_ips = ensemble::discovery::self_tailscale_ips();
    let bind = ensemble::resolve_bind(&self_ips, explicit.as_deref(), port);
    if let ensemble::BindAddr::Loopback(_) = bind {
        eprintln!(
            "ensemble: no tailnet IP found (is tailscale up?) — serving loopback only (local). \
             Pass --bind <addr> to override."
        );
    }
    let addr = bind.addr().to_string();
    let local = ensemble::present_clis();
    let hosts = ensemble::discover_mesh(port);
    println!("{}", ensemble::render_up(&addr, &local, &hosts));
    // Belt-and-suspenders flush before the long blocking serve. Rust's stdout is line-buffered
    // (LineWriter), so println!'s trailing newline already flushed the banner — this just makes the
    // ordering explicit at a blocking boundary.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    if let Err(e) = ensemble::serve_with_token(&addr, adapters(), token) {
        eprintln!("serve: {e}");
        std::process::exit(1);
    }
}

fn build_serve_service_config(
    args: &[String],
    default_exe: std::path::PathBuf,
    cwd: &Path,
) -> Result<ensemble::ServeServiceConfig, String> {
    for flag in ["--bind", "--port", "--token", "--exe"] {
        require_value_if_present(args, flag);
    }
    if has_flag(args, "--install-service") && has_flag(args, "--uninstall-service") {
        return Err("--install-service and --uninstall-service are mutually exclusive".to_string());
    }
    reject_bind_and_port(args)?;
    let exe = parse_flag(args, "--exe")
        .map(std::path::PathBuf::from)
        .unwrap_or(default_exe);
    let exe = absolutize_from(exe, cwd);
    if !exe.is_absolute() {
        return Err("--exe did not resolve to an absolute path".to_string());
    }
    Ok(ensemble::ServeServiceConfig {
        exe,
        bind: parse_flag(args, "--bind"),
        port: parse_discovery_port(args)?,
        // Only bake an explicit token into a service definition. The ambient ENSEMBLE_TOKEN for this
        // install shell may be temporary and should not silently become persistent service state.
        token: control_token_from_sources(parse_flag(args, "--token"), None),
    })
}

fn default_service_config(args: &[String]) -> ensemble::ServeServiceConfig {
    let default_exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("ensemble serve --install-service: cannot determine this binary's path ({e}) — pass --exe <path>");
        std::process::exit(2);
    });
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    build_serve_service_config(args, default_exe, &cwd).unwrap_or_else(|e| {
        eprintln!("ensemble serve: {e}");
        std::process::exit(2);
    })
}

fn serve_install_service_cmd(args: &[String]) {
    let cfg = default_service_config(args);
    let print = has_flag(args, "--print");
    if let Err(e) = install_serve_service(&cfg, print) {
        eprintln!("ensemble serve --install-service: {e}");
        std::process::exit(1);
    }
}

fn serve_uninstall_service_cmd(args: &[String]) {
    for flag in ["--bind", "--token", "--exe"] {
        require_value_if_present(args, flag);
    }
    if has_flag(args, "--install-service") {
        eprintln!(
            "ensemble serve: --install-service and --uninstall-service are mutually exclusive"
        );
        std::process::exit(2);
    }
    let cfg = default_service_config(args);
    let print = has_flag(args, "--print");
    if let Err(e) = uninstall_serve_service(&cfg, print) {
        eprintln!("ensemble serve --uninstall-service: {e}");
        std::process::exit(1);
    }
}

fn install_serve_service(cfg: &ensemble::ServeServiceConfig, print: bool) -> Result<(), String> {
    if print {
        print_service_install_plan(cfg)?;
        return Ok(());
    }

    #[cfg(windows)]
    {
        // Stopping the existing task is best-effort during install: first install or an
        // already-stopped task should still proceed to /Create /F and /Run.
        let _ = end_windows_task_if_present();
        run_command("schtasks", &ensemble::windows_install_argv(cfg))?;
        return run_command("schtasks", &ensemble::windows_run_argv());
    }
    #[cfg(target_os = "macos")]
    {
        let path = ensemble::launchd_agent_path(&service_home_dir()?);
        let dir = path
            .parent()
            .ok_or_else(|| format!("bad launchd path: {}", path.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        if path
            .try_exists()
            .map_err(|e| format!("stat {}: {e}", path.display()))?
        {
            let subject = path.display().to_string();
            run_command_path_allow_missing(
                "launchctl",
                &["unload", "-w"],
                Some(&path),
                &[subject.as_str(), ensemble::LAUNCHD_LABEL],
                &[
                    "no such file",
                    "could not find specified service",
                    "not found",
                    "not loaded",
                    "does not exist",
                ],
                &[
                    "no such file",
                    "could not find specified service",
                    "not loaded",
                ],
            )?;
        } else {
            run_command_path_allow_missing(
                "launchctl",
                &["remove", ensemble::LAUNCHD_LABEL],
                None,
                &[ensemble::LAUNCHD_LABEL],
                &[
                    "no such file",
                    "could not find specified service",
                    "not found",
                    "not loaded",
                    "does not exist",
                ],
                &["could not find specified service", "not loaded"],
            )?;
        }
        std::fs::write(&path, ensemble::launchd_plist(cfg))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        return run_command_path("launchctl", &["load", "-w"], Some(&path));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path = ensemble::systemd_user_unit_path(&service_home_dir()?);
        let dir = path
            .parent()
            .ok_or_else(|| format!("bad systemd path: {}", path.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        std::fs::write(&path, ensemble::systemd_unit(cfg))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        run_command("systemctl", &["--user".into(), "daemon-reload".into()])?;
        run_command(
            "systemctl",
            &[
                "--user".into(),
                "enable".into(),
                ensemble::SYSTEMD_UNIT_NAME.into(),
            ],
        )?;
        return run_command(
            "systemctl",
            &[
                "--user".into(),
                "restart".into(),
                ensemble::SYSTEMD_UNIT_NAME.into(),
            ],
        );
    }
    #[allow(unreachable_code)]
    Err("service install is not supported on this OS".to_string())
}

fn uninstall_serve_service(cfg: &ensemble::ServeServiceConfig, print: bool) -> Result<(), String> {
    if print {
        print_service_uninstall_plan(cfg)?;
        return Ok(());
    }

    #[cfg(windows)]
    {
        end_windows_task_if_present()?;
        return run_command_allow_missing(
            "schtasks",
            &ensemble::windows_uninstall_argv(),
            &[ensemble::WINDOWS_TASK_NAME],
            &[
                "cannot find",
                "does not exist",
                "not found",
                "不存在",
                "找不到",
            ],
            &[
                "the system cannot find the file specified",
                "系統找不到指定的檔案",
            ],
        );
    }
    #[cfg(target_os = "macos")]
    {
        let path = ensemble::launchd_agent_path(&service_home_dir()?);
        if path
            .try_exists()
            .map_err(|e| format!("stat {}: {e}", path.display()))?
        {
            let subject = path.display().to_string();
            run_command_path_allow_missing(
                "launchctl",
                &["unload", "-w"],
                Some(&path),
                &[subject.as_str(), ensemble::LAUNCHD_LABEL],
                &[
                    "no such file",
                    "could not find specified service",
                    "not found",
                    "not loaded",
                    "does not exist",
                ],
                &[
                    "no such file",
                    "could not find specified service",
                    "not loaded",
                ],
            )?;
        } else {
            run_command_path_allow_missing(
                "launchctl",
                &["remove", ensemble::LAUNCHD_LABEL],
                None,
                &[ensemble::LAUNCHD_LABEL],
                &[
                    "no such file",
                    "could not find specified service",
                    "not found",
                    "not loaded",
                    "does not exist",
                ],
                &["could not find specified service", "not loaded"],
            )?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("remove {}: {e}", path.display())),
        }
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path = ensemble::systemd_user_unit_path(&service_home_dir()?);
        run_command_allow_missing(
            "systemctl",
            &[
                "--user".into(),
                "stop".into(),
                ensemble::SYSTEMD_UNIT_NAME.into(),
            ],
            &[ensemble::SYSTEMD_UNIT_NAME],
            &["does not exist", "not found", "not loaded", "no such file"],
            &[],
        )?;
        run_command_allow_missing(
            "systemctl",
            &[
                "--user".into(),
                "disable".into(),
                ensemble::SYSTEMD_UNIT_NAME.into(),
            ],
            &[ensemble::SYSTEMD_UNIT_NAME],
            &["does not exist", "not found", "not loaded", "no such file"],
            &[],
        )?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("remove {}: {e}", path.display())),
        }
        run_command("systemctl", &["--user".into(), "daemon-reload".into()])?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("service uninstall is not supported on this OS".to_string())
}

fn print_service_install_plan(cfg: &ensemble::ServeServiceConfig) -> Result<(), String> {
    #[cfg(windows)]
    {
        println!("program: schtasks");
        for args in [
            ensemble::windows_end_argv(),
            ensemble::windows_install_argv(cfg),
            ensemble::windows_run_argv(),
        ] {
            println!("args: {}", args.join(" "));
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let path = ensemble::launchd_agent_path(&service_home_dir()?);
        if path
            .try_exists()
            .map_err(|e| format!("stat {}: {e}", path.display()))?
        {
            println!("command: launchctl unload -w {}", path.display());
        } else {
            println!("command: launchctl remove {}", ensemble::LAUNCHD_LABEL);
        }
        println!("path: {}", path.display());
        print!("{}", ensemble::launchd_plist(cfg));
        println!("command: launchctl load -w {}", path.display());
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path = ensemble::systemd_user_unit_path(&service_home_dir()?);
        println!("path: {}", path.display());
        print!("{}", ensemble::systemd_unit(cfg));
        println!("command: systemctl --user daemon-reload");
        println!(
            "command: systemctl --user enable {}",
            ensemble::SYSTEMD_UNIT_NAME
        );
        println!(
            "command: systemctl --user restart {}",
            ensemble::SYSTEMD_UNIT_NAME
        );
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("service install is not supported on this OS".to_string())
}

fn print_service_uninstall_plan(_cfg: &ensemble::ServeServiceConfig) -> Result<(), String> {
    #[cfg(windows)]
    {
        println!("program: schtasks");
        for args in [
            ensemble::windows_end_argv(),
            ensemble::windows_uninstall_argv(),
        ] {
            println!("args: {}", args.join(" "));
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let path = ensemble::launchd_agent_path(&service_home_dir()?);
        if path
            .try_exists()
            .map_err(|e| format!("stat {}: {e}", path.display()))?
        {
            println!("command: launchctl unload -w {}", path.display());
        } else {
            println!("command: launchctl remove {}", ensemble::LAUNCHD_LABEL);
        }
        println!("remove: {}", path.display());
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path = ensemble::systemd_user_unit_path(&service_home_dir()?);
        println!(
            "command: systemctl --user stop {}",
            ensemble::SYSTEMD_UNIT_NAME
        );
        println!(
            "command: systemctl --user disable {}",
            ensemble::SYSTEMD_UNIT_NAME
        );
        println!("remove: {}", path.display());
        println!("command: systemctl --user daemon-reload");
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("service uninstall is not supported on this OS".to_string())
}

#[cfg(unix)]
fn service_home_dir() -> Result<std::path::PathBuf, String> {
    let home = home_dir();
    if home.is_absolute() {
        Ok(home)
    } else {
        Err("could not determine an absolute home directory for the user service".to_string())
    }
}

#[cfg(windows)]
fn end_windows_task_if_present() -> Result<(), String> {
    run_command_allow_missing(
        "schtasks",
        &ensemble::windows_end_argv(),
        &[ensemble::WINDOWS_TASK_NAME],
        &[
            "cannot find",
            "does not exist",
            "not found",
            "not currently running",
            "currently not running",
            "not running",
            "不存在",
            "找不到",
            "未執行",
            "沒有執行",
        ],
        &[
            "the system cannot find the file specified",
            "系統找不到指定的檔案",
            "the task is not currently running",
            "工作目前並未執行",
            "工作未執行",
        ],
    )
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn run_command(program: &str, args: &[String]) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format_command_failure(program, args, &output))
}

#[cfg(target_os = "macos")]
fn run_command_path(program: &str, args: &[&str], path: Option<&Path>) -> Result<(), String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    if let Some(path) = path {
        cmd.arg(path);
    }
    let output = cmd.output().map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let mut display_args = args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if let Some(path) = path {
        display_args.push(path.display().to_string());
    }
    Err(format_command_failure(program, &display_args, &output))
}

#[cfg(target_os = "macos")]
fn run_command_path_allow_missing(
    program: &str,
    args: &[&str],
    path: Option<&Path>,
    expected_subjects: &[&str],
    missing_markers: &[&str],
    scoped_missing_markers: &[&str],
) -> Result<(), String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    if let Some(path) = path {
        cmd.arg(path);
    }
    let output = cmd.output().map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    if output_contains_missing_marker(
        &output,
        expected_subjects,
        missing_markers,
        scoped_missing_markers,
    ) {
        return Ok(());
    }
    let mut display_args = args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if let Some(path) = path {
        display_args.push(path.display().to_string());
    }
    Err(format_command_failure(program, &display_args, &output))
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn run_command_allow_missing(
    program: &str,
    args: &[String],
    expected_subjects: &[&str],
    missing_markers: &[&str],
    scoped_missing_markers: &[&str],
) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    if output_contains_missing_marker(
        &output,
        expected_subjects,
        missing_markers,
        scoped_missing_markers,
    ) {
        return Ok(());
    }
    Err(format_command_failure(program, args, &output))
}

fn output_contains_missing_marker(
    output: &std::process::Output,
    expected_subjects: &[&str],
    missing_markers: &[&str],
    scoped_missing_markers: &[&str],
) -> bool {
    text_contains_missing_marker(
        &command_output_text(output),
        expected_subjects,
        missing_markers,
        scoped_missing_markers,
    )
}

fn text_contains_missing_marker(
    text: &str,
    expected_subjects: &[&str],
    missing_markers: &[&str],
    scoped_missing_markers: &[&str],
) -> bool {
    let text = text.to_lowercase();
    let subject_matches = expected_subjects
        .iter()
        .any(|subject| text.contains(&subject.to_lowercase()));
    (subject_matches
        && missing_markers
            .iter()
            .any(|marker| text.contains(&marker.to_lowercase())))
        || scoped_missing_markers
            .iter()
            .any(|marker| text.contains(&marker.to_lowercase()))
}

fn format_command_failure(program: &str, args: &[String], output: &std::process::Output) -> String {
    format!(
        "{} {} exited {}: {}",
        program,
        args.join(" "),
        output.status,
        command_output_text(output).trim()
    )
}

fn command_output_text(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}{stderr}")
}

/// `ensemble watch <member[@node]> [--repo <p>] [--node <host|url>] [--team <name>] [--token <token>] [--since <n>] [--follow] [--json]` — tail a member's stream feed
/// (.ensemble/stream/<member>.ndjson), rendering each event. Read-only. `--follow` polls for new events
/// until Ctrl-C.
fn watch_cmd(args: &[String]) {
    require_value_if_present(args, "--repo");
    require_value_if_present(args, "--node");
    require_value_if_present(args, "--port");
    require_value_if_present(args, "--team");
    require_value_if_present(args, "--token");
    require_value_if_present(args, "--since");
    let port = parse_control_port_or_exit(args);
    let w = ensemble::parse_watch_args(args);
    let member = match w.member {
        Some(m) => m,
        None => {
            eprintln!(
                "usage: ensemble watch <member> [--repo <p>] [--node <host|url>] [--port <n>] [--team <name>] [--since <n>] [--follow] [--json]"
            );
            std::process::exit(2);
        }
    };
    let routed = route_control_member_discovering(&member, w.node.as_deref(), port);
    let repo = w.repo.map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    let token = control_token(args);
    let plane = control_plane(routed.node.as_deref(), token.as_deref(), port).unwrap_or_else(|e| {
        eprintln!("ensemble watch: {e}");
        std::process::exit(2);
    });
    let stream_target = control_stream_target(&repo, &routed.member, routed.node.as_deref());

    // Read everything from `cursor` onward, render it, and advance the cursor PAST each line only after
    // it is actually written — so a failed write never silently drops an event by counting it as done. A
    // missing feed reads as empty (`watch` just waits). Flush each batch: under `--follow` stdout is
    // block-buffered when piped/redirected, so without an explicit flush the tailed lines wouldn't appear
    // promptly. Propagates the io::Error (feed read OR stdout write) for the caller to classify.
    let drain = |cursor: &mut usize| -> std::io::Result<()> {
        let lines = plane.read_stream(&repo, &routed.member, *cursor)?;
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        for l in &lines {
            if w.json {
                writeln!(out, "{l}")?;
            } else {
                writeln!(out, "{}", ensemble::render_line(l))?;
            }
            *cursor += 1;
        }
        out.flush()
    };

    // One drain pass; classify any error. A closed downstream pipe (BrokenPipe, e.g. `ensemble watch
    // --follow | head`) is a normal end-of-consumer — exit 0, not a failure. Any other read/write error
    // exits non-zero. On Windows a closed pipe can surface as ERROR_NO_DATA (raw os error 232) rather
    // than the BrokenPipe kind, so treat that as a clean close too. (Like `tail -f`, a gone consumer is
    // detected on the NEXT write: a live feed exits promptly when the next event can't be written; a
    // fully-quiescent feed keeps polling until the next event or Ctrl-C.)
    let step = |cursor: &mut usize| {
        if let Err(e) = drain(cursor) {
            let closed =
                e.kind() == std::io::ErrorKind::BrokenPipe || e.raw_os_error() == Some(232);
            if closed {
                std::process::exit(0);
            }
            eprintln!("ensemble watch: {stream_target}: {e}");
            std::process::exit(1);
        }
    };

    let mut cursor = w.since;
    step(&mut cursor);
    if !w.follow {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        step(&mut cursor);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuperviseCliArgs {
    name: String,
    repo: String,
    team: Option<String>,
    agent: String,
    since: usize,
    json: bool,
    apply_steer: bool,
    abort_on_critical: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SuperviseCliOutput {
    ok: bool,
    name: String,
    team: String,
    agent: String,
    recommendation: Option<ensemble::SupervisorRecommendation>,
    reason: String,
    steer: Option<String>,
    critical: bool,
    board_next: Option<usize>,
    control_next: Option<usize>,
    error_kind: Option<String>,
}

fn parse_supervise_args(args: &[String]) -> Result<SuperviseCliArgs, String> {
    let mut name = None;
    let mut repo = ".".to_string();
    let mut team = None;
    let mut agent = "claude".to_string();
    let mut since = 0usize;
    let mut json = false;
    let mut apply_steer = false;
    let mut abort_on_critical = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => {
                repo = take_member_flag_value(args, &mut i, "--repo")?;
            }
            "--team" => {
                team = Some(take_member_flag_value(args, &mut i, "--team")?);
            }
            "--agent" => {
                agent = take_member_flag_value(args, &mut i, "--agent")?;
            }
            "--since" => {
                let raw = take_member_flag_value(args, &mut i, "--since")?;
                since = raw
                    .parse::<usize>()
                    .map_err(|_| "--since must be a non-negative integer".to_string())?;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "--apply-steer" => {
                apply_steer = true;
                i += 1;
            }
            "--abort-on-critical" => {
                abort_on_critical = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            value => {
                if name.is_some() {
                    return Err(format!("unexpected positional argument `{value}`"));
                }
                name = Some(value.to_string());
                i += 1;
            }
        }
    }
    let name = name.ok_or_else(|| "ensemble supervise needs <name>".to_string())?;
    let agent = agent.trim();
    if agent.is_empty() {
        return Err("--agent must not be blank".to_string());
    }
    Ok(SuperviseCliArgs {
        name,
        repo,
        team,
        agent: agent.to_string(),
        since,
        json,
        apply_steer,
        abort_on_critical,
    })
}

fn supervise_apply_mode(parsed: &SuperviseCliArgs) -> ensemble::SupervisorApply {
    match (parsed.apply_steer, parsed.abort_on_critical) {
        (true, true) => ensemble::SupervisorApply::ApplySteerAndAbortOnCritical,
        (true, false) => ensemble::SupervisorApply::ApplySteer,
        (false, true) => ensemble::SupervisorApply::AbortOnCritical,
        (false, false) => ensemble::SupervisorApply::Advisory,
    }
}

fn supervise_cmd(args: &[String]) {
    let parsed = parse_supervise_args(args).unwrap_or_else(|e| {
        eprintln!("ensemble supervise: {e}");
        std::process::exit(2);
    });
    let repo = absolutize(std::path::PathBuf::from(&parsed.repo));
    let evidence = ensemble::collect_supervisor_evidence(
        &repo,
        parsed.team.as_deref(),
        &parsed.name,
        parsed.since,
        50,
    )
    .unwrap_or_else(|e| {
        eprintln!("ensemble supervise: collect evidence: {e}");
        std::process::exit(1);
    });
    let prompt = ensemble::build_supervisor_prompt(&evidence);
    let (adapter, _label) = resolve_one(&parsed.agent, None, false).unwrap_or_else(|| {
        eprintln!(
            "ensemble supervise: no local adapter for agent '{}'",
            parsed.agent
        );
        std::process::exit(2);
    });
    let session = ensemble::resolve_team_session(
        &repo,
        Some(&evidence.team),
        "supervisor",
        Some("supervisor"),
        None,
    );

    let raw = match adapter.run(&prompt, &repo) {
        Ok(out) => out.text,
        Err(e) => {
            let kind = adapter_error_kind(&e).to_string();
            let body = format!("supervisor `{}` flaked: {e}", parsed.agent);
            let board_next =
                ensemble::post_team_message(&session, "supervisor", "flake", &body).ok();
            let output = SuperviseCliOutput {
                ok: false,
                name: parsed.name,
                team: evidence.team,
                agent: parsed.agent,
                recommendation: None,
                reason: body,
                steer: None,
                critical: false,
                board_next,
                control_next: None,
                error_kind: Some(kind),
            };
            if parsed.json {
                println!("{}", serde_json::to_string(&output).unwrap_or_default());
            } else {
                eprintln!("{}", output.reason);
            }
            std::process::exit(e.exit_code());
        }
    };

    let report =
        ensemble::parse_supervisor_report(&raw).unwrap_or_else(|e| ensemble::SupervisorReport {
            recommendation: ensemble::SupervisorRecommendation::NeedsHuman,
            reason: format!("unparseable supervisor output: {e}"),
            steer: None,
            critical: false,
        });
    let body = format!(
        "supervise `{}` via `{}`: {:?} - {}",
        parsed.name, parsed.agent, report.recommendation, report.reason
    );
    let board_next = ensemble::post_team_message(&session, "supervisor", "supervise", &body)
        .unwrap_or_else(|e| {
            eprintln!("ensemble supervise: post board result: {e}");
            std::process::exit(1);
        });
    let action =
        ensemble::control_action_for_report(&report, supervise_apply_mode(&parsed), "supervisor");
    let control_next = action
        .as_ref()
        .map(|cmd| append_control_direct(&repo, None, None, DEFAULT_SERVE_PORT, &parsed.name, cmd))
        .transpose()
        .unwrap_or_else(|e| {
            eprintln!("ensemble supervise: {e}");
            std::process::exit(1);
        });
    let output = SuperviseCliOutput {
        ok: true,
        name: parsed.name,
        team: evidence.team,
        agent: parsed.agent,
        recommendation: Some(report.recommendation),
        reason: report.reason,
        steer: report.steer,
        critical: report.critical,
        board_next: Some(board_next),
        control_next,
        error_kind: None,
    };
    if parsed.json {
        println!("{}", serde_json::to_string(&output).unwrap_or_default());
    } else {
        println!(
            "supervise `{}`: {:?} - {}",
            output.name,
            output
                .recommendation
                .unwrap_or(ensemble::SupervisorRecommendation::NeedsHuman),
            output.reason
        );
        if let Some(next) = output.control_next {
            println!("control next={next}");
        }
    }
}

/// `ensemble steer <name[@node]> "<prompt>" [--repo <p>] [--node <host|url>] [--from <id>]` — inject an operator redirect into the
/// NEXT round of the live run started with `--watch <name>` (keeps a drifting CLI on track). Appends a
/// Steer to that run's control feed; the run's watcher picks it up at the next round boundary.
fn steer_cmd(args: &[String]) {
    require_value_if_present(args, "--repo");
    require_value_if_present(args, "--node");
    require_value_if_present(args, "--port");
    require_value_if_present(args, "--token");
    require_value_if_present(args, "--from");
    let (name, prompt) = match positional_tasks(args).as_slice() {
        [name, prompt] => (name.clone(), prompt.clone()),
        _ => {
            eprintln!(
                "usage: ensemble steer <name> \"<prompt>\" [--repo <p>] [--node <host|url>] [--port <n>] [--from <id>]"
            );
            std::process::exit(2);
        }
    };
    let from = parse_flag(args, "--from").unwrap_or_else(|| "operator".to_string());
    append_control(args, &name, &ensemble::ControlCmd::Steer { from, prompt });
    println!("steered `{name}`");
}

/// `ensemble abort <name[@node]> [--hard] [--repo <p>] [--node <host|url>] [--from <id>]` — stop the live run started with
/// `--watch <name>`: cleanly at the next round boundary, or `--hard` to kill the running CLI now.
fn abort_cmd(args: &[String]) {
    require_value_if_present(args, "--repo");
    require_value_if_present(args, "--node");
    require_value_if_present(args, "--port");
    require_value_if_present(args, "--token");
    require_value_if_present(args, "--from");
    let name = match positional_tasks(args).first() {
        Some(n) => n.clone(),
        None => {
            eprintln!(
                "usage: ensemble abort <name> [--hard] [--repo <p>] [--node <host|url>] [--port <n>] [--from <id>]"
            );
            std::process::exit(2);
        }
    };
    let from = parse_flag(args, "--from").unwrap_or_else(|| "operator".to_string());
    let hard = has_flag(args, "--hard");
    append_control(args, &name, &ensemble::ControlCmd::Abort { from, hard });
    println!("{} `{name}`", if hard { "hard-aborted" } else { "aborted" });
}

/// Append a control command to `<repo>/.ensemble/control/<name>.ndjson` (`--repo` defaults to cwd).
fn append_control(args: &[String], name: &str, cmd: &ensemble::ControlCmd) {
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let explicit_node = parse_flag(args, "--node");
    let port = parse_control_port_or_exit(args);
    let routed = route_control_member_discovering(name, explicit_node.as_deref(), port);
    let token = control_token(args);
    if let Err(e) = append_control_direct(
        Path::new(&repo),
        routed.node.as_deref(),
        token.as_deref(),
        port,
        &routed.member,
        cmd,
    ) {
        eprintln!("ensemble: {e}");
        std::process::exit(1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoutedControlMember {
    member: String,
    node: Option<String>,
}

fn route_control_member(
    member: &str,
    explicit_node: Option<&str>,
    raw_host: Option<&str>,
    mesh: &[(String, Vec<String>)],
) -> RoutedControlMember {
    let member = member.to_string();
    let node = match explicit_node {
        Some(node) => explicit_control_node(node),
        None => inferred_member_node(&member, raw_host)
            .map(|node| discovered_control_node(&node, mesh).unwrap_or(node)),
    };
    RoutedControlMember { member, node }
}

fn route_control_member_discovering(
    member: &str,
    explicit_node: Option<&str>,
    port: u16,
) -> RoutedControlMember {
    let raw_host = raw_hostname();
    let mesh =
        if explicit_node.is_none() && inferred_member_node(member, raw_host.as_deref()).is_some() {
            ensemble::discover_mesh(port)
        } else {
            Vec::new()
        };
    route_control_member(member, explicit_node, raw_host.as_deref(), &mesh)
}

fn inferred_member_node(member: &str, raw_host: Option<&str>) -> Option<String> {
    let (prefix, node) = member.rsplit_once('@')?;
    let node = node.trim();
    if prefix.trim().is_empty() || node.is_empty() || is_local_member_node(node, raw_host) {
        None
    } else {
        Some(node.to_string())
    }
}

fn explicit_control_node(node: &str) -> Option<String> {
    let node = node.trim();
    if is_local_control_escape(node) {
        None
    } else {
        Some(node.to_string())
    }
}

fn discovered_control_node(node: &str, mesh: &[(String, Vec<String>)]) -> Option<String> {
    let wanted = normalized_control_host(node)?;
    mesh.iter().find_map(|(url, _agents)| {
        let host = ensemble::short_host(url);
        (normalized_control_host(&host).as_deref() == Some(wanted.as_str())).then(|| url.clone())
    })
}

fn is_local_member_node(node: &str, raw_host: Option<&str>) -> bool {
    let Some(node) = normalized_control_host(node) else {
        return false;
    };
    if matches!(
        node.as_str(),
        "local" | "localhost" | "127.0.0.1" | "::1" | "[::1]"
    ) {
        return true;
    }
    normalized_control_host(raw_host.unwrap_or(""))
        .map(|host| host == node)
        .unwrap_or(false)
}

fn normalized_control_host(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if matches!(lower.as_str(), "local" | "localhost") {
        return Some(lower);
    }
    if matches!(raw, "127.0.0.1" | "::1" | "[::1]") {
        return Some(raw.to_string());
    }
    let host = ensemble::short_host(raw);
    let is_ipv4 = host.contains('.')
        && host
            .split('.')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()));
    if is_ipv4 || host.starts_with('[') {
        return Some(host.to_ascii_lowercase());
    }
    let short = ensemble::default_member_name("probe", Some(&host));
    short.strip_prefix("probe@").map(str::to_string)
}

fn append_control_direct(
    repo: &Path,
    node: Option<&str>,
    token: Option<&str>,
    port: u16,
    name: &str,
    cmd: &ensemble::ControlCmd,
) -> Result<usize, String> {
    let plane = control_plane(node, token, port)?;
    let target = control_control_target(repo, name, node);
    plane
        .append_control(repo, name, cmd)
        .map_err(|e| format!("write control feed {target}: {e}"))
}

fn control_plane(
    node: Option<&str>,
    token: Option<&str>,
    port: u16,
) -> Result<Box<dyn ensemble::ControlPlane>, String> {
    match node {
        Some(node) if is_local_control_escape(node) => {
            Ok(Box::new(ensemble::LocalControlPlane::new()))
        }
        Some(node) => {
            let url = control_node_url(node, port)?;
            if let Some(token) = token {
                Ok(Box::new(ensemble::RemoteControlPlane::with_token(
                    &url, token,
                )))
            } else {
                Ok(Box::new(ensemble::RemoteControlPlane::new(&url)))
            }
        }
        None => Ok(Box::new(ensemble::LocalControlPlane::new())),
    }
}

fn control_token(args: &[String]) -> Option<String> {
    control_token_from_sources(parse_flag(args, "--token"), control_token_env())
}

fn control_token_from_sources(explicit: Option<String>, env: Option<String>) -> Option<String> {
    explicit
        .and_then(normalize_control_token)
        .or_else(|| env.and_then(normalize_control_token))
}

fn control_token_env() -> Option<String> {
    std::env::var("ENSEMBLE_TOKEN").ok()
}

fn normalize_control_token(token: String) -> Option<String> {
    if token.chars().any(char::is_control) {
        return None;
    }
    let token = token.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn control_node_url(raw: &str, port: u16) -> Result<String, String> {
    let node = raw.trim();
    if node.is_empty() {
        return Err("--node requires a non-empty value".to_string());
    }
    if node.eq_ignore_ascii_case("auto") {
        return Err(
            "--node auto is not supported for control commands yet; use a host or URL".to_string(),
        );
    }
    if node.chars().any(char::is_control) {
        return Err("--node must not contain control characters".to_string());
    }
    let node = node.trim_end_matches('/');
    if node.starts_with("http://") || node.starts_with("https://") {
        Ok(node.to_string())
    } else if node.starts_with('[') {
        if node.contains("]:") {
            Ok(format!("http://{node}"))
        } else {
            Ok(format!("http://{node}:{port}"))
        }
    } else if let Some((host, raw_port)) = node.rsplit_once(':') {
        let has_single_colon = !host.contains(':');
        let has_port = !host.is_empty() && raw_port.chars().all(|c| c.is_ascii_digit());
        if has_single_colon && has_port {
            Ok(format!("http://{node}"))
        } else {
            Ok(format!("http://[{node}]:{port}"))
        }
    } else {
        Ok(format!("http://{node}:{port}"))
    }
}

fn is_local_control_escape(node: &str) -> bool {
    node.trim().eq_ignore_ascii_case("local")
}

fn control_stream_target(repo: &Path, name: &str, node: Option<&str>) -> String {
    match node {
        Some(node) => format!(
            "remote `{node}` stream `{name}` for repo {}",
            repo.display()
        ),
        None => ensemble::member_stream_path(repo, name)
            .display()
            .to_string(),
    }
}

fn control_control_target(repo: &Path, name: &str, node: Option<&str>) -> String {
    match node {
        Some(node) => format!(
            "remote `{node}` control `{name}` for repo {}",
            repo.display()
        ),
        None => ensemble::member_control_path(repo, name)
            .display()
            .to_string(),
    }
}

/// `ensemble all "<prompt>" [--repo <p>] [--no-discover] [--json]` — COUNCIL broadcast: fan the SAME
/// prompt to EVERY AI CLI ensemble can reach (local CLIs on PATH + every agent on every tailnet peer
/// running `ensemble serve`), each as ONE read-only turn, then print every reply side by side. No
/// worktree, no gate, no land — pure compare-the-fleet-on-one-question (item 0.7). `--no-discover` = local.
fn all_cmd(args: &[String]) {
    require_value_if_present(args, "--repo");
    let prompt = match positional_tasks(args).into_iter().next() {
        Some(p) => p,
        None => {
            eprintln!("usage: ensemble all \"<prompt>\" [--repo <p>] [--no-discover] [--json]");
            std::process::exit(2);
        }
    };
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let local = ensemble::present_clis();
    let mesh = if has_flag(args, "--no-discover") {
        Vec::new()
    } else {
        ensemble::discover_mesh(7878)
    };
    let targets = ensemble::council_targets(&local, &mesh);
    if targets.is_empty() {
        eprintln!("ensemble all: no AI CLIs found (local PATH or tailnet)");
        std::process::exit(1);
    }
    // Fan out: one read-only turn per target, in parallel. Scoped threads borrow the prompt/cwd/targets.
    let prompt_ref = prompt.as_str();
    let cwd = Path::new(&repo);
    let results: Vec<(String, Result<String, String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .iter()
            .map(|t| s.spawn(move || council_run_one(t, prompt_ref, cwd)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| ("?".to_string(), Err("worker panicked".to_string())))
            })
            .collect()
    });
    if has_flag(args, "--json") {
        let arr: Vec<serde_json::Value> = results
            .iter()
            .map(|(label, r)| match r {
                Ok(t) => serde_json::json!({"label": label, "ok": true, "text": t}),
                Err(e) => serde_json::json!({"label": label, "ok": false, "error": e}),
            })
            .collect();
        println!("{}", serde_json::to_string(&arr).unwrap_or_default());
    } else {
        print!("{}", ensemble::render_council(&results));
    }
}

/// `ensemble team <status|say|inbox>` — inspect/post to the repo-local team state without starting a
/// vendor CLI. This is the operator-facing shell around `team.rs`; MCP tools reuse the same data API.
fn team_cmd(args: &[String]) {
    let parsed = parse_team_cmd_args(args).unwrap_or_else(|e| {
        eprintln!(
            "ensemble team: {e}\nusage: ensemble team <status|say|inbox> [--repo <p>] [--team <name>] [--node <host|url>] [--port <n>] [--token <token>] [--json]"
        );
        std::process::exit(2);
    });
    let repo = absolutize(std::path::PathBuf::from(&parsed.repo));
    let session = ensemble::resolve_team_session(
        &repo,
        parsed.team.as_deref(),
        "operator",
        Some("operator"),
        None,
    );
    let token = control_token_from_sources(parsed.token.clone(), control_token_env());
    let plane = control_plane(parsed.node.as_deref(), token.as_deref(), parsed.port)
        .unwrap_or_else(|e| {
            eprintln!("ensemble team: {e}");
            std::process::exit(2);
        });

    match parsed.action {
        TeamCliAction::Status => {
            let status = plane.team_status(&session).unwrap_or_else(|e| {
                eprintln!("ensemble team status: {e}");
                std::process::exit(1);
            });
            if parsed.json {
                println!("{}", serde_json::to_string(&status).unwrap_or_default());
            } else {
                println!("{}", ensemble::render_team_status(&status));
            }
        }
        TeamCliAction::Say { from, message } => {
            let cursor = plane
                .post_team_message(&session, &from, "note", &message)
                .unwrap_or_else(|e| {
                    eprintln!("ensemble team say: {e}");
                    std::process::exit(1);
                });
            println!("posted team message next={cursor}");
        }
        TeamCliAction::Inbox { since } => {
            let inbox = plane.read_team_inbox(&session, since).unwrap_or_else(|e| {
                eprintln!("ensemble team inbox: {e}");
                std::process::exit(1);
            });
            if parsed.json {
                println!("{}", serde_json::to_string(&inbox).unwrap_or_default());
            } else {
                println!("{}", ensemble::render_team_inbox(&inbox));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TeamCliArgs {
    action: TeamCliAction,
    repo: String,
    team: Option<String>,
    node: Option<String>,
    port: u16,
    token: Option<String>,
    json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TeamCliAction {
    Status,
    Say { from: String, message: String },
    Inbox { since: usize },
}

fn parse_team_cmd_args(args: &[String]) -> Result<TeamCliArgs, String> {
    let action = args
        .get(2)
        .map(String::as_str)
        .ok_or_else(|| "missing subcommand (expected status | say | inbox)".to_string())?;
    let mut repo = ".".to_string();
    let mut team = None;
    let mut node = None;
    let mut port = DEFAULT_SERVE_PORT;
    let mut token = None;
    let mut from = None;
    let mut since = None;
    let mut json = false;
    let mut positionals = Vec::new();
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => {
                repo = take_team_flag_value(args, &mut i, "--repo")?;
            }
            "--team" => {
                team = Some(take_team_flag_value(args, &mut i, "--team")?);
            }
            "--node" => {
                node = Some(take_team_flag_value(args, &mut i, "--node")?);
            }
            "--port" => {
                let raw = take_team_flag_value(args, &mut i, "--port")?;
                port = parse_control_port_value(&raw)?;
            }
            "--token" => {
                token = Some(take_team_flag_value(args, &mut i, "--token")?);
            }
            "--from" => {
                from = Some(take_team_flag_value(args, &mut i, "--from")?);
            }
            "--since" => {
                let raw = take_team_flag_value(args, &mut i, "--since")?;
                since = Some(
                    raw.parse::<usize>()
                        .map_err(|_| "--since must be a non-negative integer".to_string())?,
                );
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            value => {
                positionals.push(value.to_string());
                i += 1;
            }
        }
    }

    let action = match action {
        "status" => {
            if from.is_some() {
                return Err("--from is only valid for team say".to_string());
            }
            if since.is_some() {
                return Err("--since is only valid for team inbox".to_string());
            }
            if !positionals.is_empty() {
                return Err("team status takes no positional arguments".to_string());
            }
            TeamCliAction::Status
        }
        "say" => {
            if json {
                return Err("--json is not valid for team say".to_string());
            }
            if since.is_some() {
                return Err("--since is only valid for team inbox".to_string());
            }
            let [message] = positionals.as_slice() else {
                return Err("team say needs exactly one message".to_string());
            };
            TeamCliAction::Say {
                from: from.unwrap_or_else(|| "operator".to_string()),
                message: message.clone(),
            }
        }
        "inbox" => {
            if from.is_some() {
                return Err("--from is only valid for team say".to_string());
            }
            if !positionals.is_empty() {
                return Err("team inbox takes no positional arguments".to_string());
            }
            TeamCliAction::Inbox {
                since: since.unwrap_or(0),
            }
        }
        other => {
            return Err(format!(
                "unknown subcommand `{other}` (expected status | say | inbox)"
            ));
        }
    };

    Ok(TeamCliArgs {
        action,
        repo,
        team,
        node,
        port,
        token,
        json,
    })
}

fn take_team_flag_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*i + 1)
        .filter(|v| !v.starts_with("--"))
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))?;
    *i += 2;
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmPolicy {
    Ask,
    Approve,
    Deny,
}

impl ConfirmPolicy {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "ask" => Ok(Self::Ask),
            "approve" => Ok(Self::Approve),
            "deny" => Ok(Self::Deny),
            _ => Err("--confirm-policy must be one of ask, approve, deny".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Approve => "approve",
            Self::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LauncherKind {
    Member(ensemble::mcp_install::ClientKind),
    Agy,
}

impl LauncherKind {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "codex" => Some(Self::Member(ensemble::mcp_install::ClientKind::Codex)),
            "claude" => Some(Self::Member(ensemble::mcp_install::ClientKind::Claude)),
            "opencode" => Some(Self::Member(ensemble::mcp_install::ClientKind::Opencode)),
            "agy" => Some(Self::Agy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LauncherInvocation {
    Member {
        client: ensemble::mcp_install::ClientKind,
        args: MemberLauncherArgs,
    },
    Agy(AgyCliArgs),
}

fn parse_launcher_invocation(args: &[String]) -> Result<Option<LauncherInvocation>, String> {
    if args.len() <= 1 {
        return Ok(None);
    }
    let Some(first) = args.get(1) else {
        return Ok(None);
    };
    if LauncherKind::parse(first).is_none() && !is_launcher_leading_flag(first) {
        if first.starts_with("--") {
            return Err(format!("unknown flag `{first}`"));
        }
        return Ok(None);
    }

    let (launcher_idx, kind) = find_launcher_token(args)?.ok_or_else(|| {
        "expected one of codex, claude, opencode, or agy after ensemble launcher options"
            .to_string()
    })?;
    match kind {
        LauncherKind::Member(client) => Ok(Some(LauncherInvocation::Member {
            client,
            args: parse_member_launcher_args_at(args, launcher_idx)?,
        })),
        LauncherKind::Agy => Ok(Some(LauncherInvocation::Agy(parse_agy_args_at(
            args,
            launcher_idx,
        )?))),
    }
}

fn find_launcher_token(args: &[String]) -> Result<Option<(usize, LauncherKind)>, String> {
    let mut i = 1;
    while i < args.len() {
        let token = &args[i];
        if let Some(kind) = LauncherKind::parse(token) {
            return Ok(Some((i, kind)));
        }
        if !is_launcher_leading_flag(token) {
            return Err(format!(
                "unexpected argument `{token}` before launcher; expected ensemble options followed by codex|claude|opencode|agy"
            ));
        }
        i += if launcher_flag_takes_value(token) {
            if args
                .get(i + 1)
                .filter(|v| !v.starts_with("--") && LauncherKind::parse(v).is_none())
                .is_none()
            {
                return Err(format!("{token} requires a value"));
            }
            2
        } else {
            1
        };
    }
    Ok(None)
}

fn is_launcher_leading_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--repo"
            | "--team"
            | "--member"
            | "--name"
            | "--confirm-policy"
            | "--print-config"
            | "--timeout"
            | "--prompt"
            | "--print-prompt"
            | "--json"
    )
}

fn launcher_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--repo" | "--team" | "--member" | "--name" | "--confirm-policy" | "--timeout" | "--prompt"
    )
}

fn reject_old_member_tail(launcher: &str, vendor_args: &[String]) -> Result<(), String> {
    if vendor_args.first().is_some_and(|a| a == "--") {
        return Err(format!(
            "old `--` separator syntax was removed; put vendor args directly after `{launcher}`"
        ));
    }
    Ok(())
}

fn member_confirmation_args(
    client: ensemble::mcp_install::ClientKind,
    policy: ConfirmPolicy,
) -> Result<Vec<String>, String> {
    let args = match (client, policy) {
        (_, ConfirmPolicy::Ask) => Vec::new(),
        (ensemble::mcp_install::ClientKind::Codex, ConfirmPolicy::Approve) => {
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
        }
        (ensemble::mcp_install::ClientKind::Codex, ConfirmPolicy::Deny) => vec![
            "--sandbox".to_string(),
            "read-only".to_string(),
            "--ask-for-approval".to_string(),
            "never".to_string(),
        ],
        (ensemble::mcp_install::ClientKind::Claude, ConfirmPolicy::Approve) => {
            vec!["--dangerously-skip-permissions".to_string()]
        }
        (ensemble::mcp_install::ClientKind::Claude, ConfirmPolicy::Deny) => {
            vec!["--permission-mode".to_string(), "dontAsk".to_string()]
        }
        (ensemble::mcp_install::ClientKind::Opencode, unsupported) => {
            return Err(format!(
                "`opencode --help` does not expose a stable non-interactive confirmation flag; \
                 --confirm-policy {} is unsupported for opencode",
                unsupported.as_str()
            ));
        }
    };
    Ok(args)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemberLauncherArgs {
    repo: String,
    team: String,
    name: Option<String>,
    vendor_args: Vec<String>,
    confirm_policy: ConfirmPolicy,
    print_config: bool,
}

#[derive(Debug, Clone)]
struct MemberLauncherEnv {
    cwd: std::path::PathBuf,
    exe: std::path::PathBuf,
    raw_host: Option<String>,
    home: std::path::PathBuf,
    codex_home: Option<std::path::PathBuf>,
    /// Explicit vendor-binary override (from `ENSEMBLE_<CLIENT>_BIN`). When set, the
    /// controlled launcher spawns this exact program instead of resolving the bare
    /// client name through PATH. Needed when a real same-named CLI is installed but a
    /// run (or the hermetic acceptance test) must drive a specific or fake binary.
    vendor_bin: Option<String>,
}

/// Environment variable an operator can set to pin the vendor binary the controlled
/// launcher spawns for `client`, e.g. `ENSEMBLE_CODEX_BIN`.
fn vendor_bin_env_key(client: ensemble::mcp_install::ClientKind) -> String {
    format!("ENSEMBLE_{}_BIN", client.as_str().to_uppercase())
}

/// Read the per-client vendor-binary override from this process's environment.
fn vendor_bin_override(client: ensemble::mcp_install::ClientKind) -> Option<String> {
    std::env::var(vendor_bin_env_key(client))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone)]
struct MemberLaunchPlan {
    client: ensemble::mcp_install::ClientKind,
    repo: std::path::PathBuf,
    team: String,
    member: String,
    crew: std::path::PathBuf,
    config_path: std::path::PathBuf,
    session: ensemble::TeamSession,
    params: ensemble::mcp_install::InstallParams,
    vendor_program: String,
    vendor_args: Vec<String>,
    confirm_policy: ConfirmPolicy,
    print_config: bool,
}

#[cfg(test)]
fn parse_member_launcher_args(args: &[String]) -> Result<MemberLauncherArgs, String> {
    let (launcher_idx, kind) = find_launcher_token(args)?.ok_or_else(|| {
        "expected codex, claude, or opencode after ensemble launcher options".to_string()
    })?;
    match kind {
        LauncherKind::Member(_) => parse_member_launcher_args_at(args, launcher_idx),
        LauncherKind::Agy => Err("expected codex, claude, or opencode; got agy".to_string()),
    }
}

fn parse_member_launcher_args_at(
    args: &[String],
    launcher_idx: usize,
) -> Result<MemberLauncherArgs, String> {
    let mut repo = ".".to_string();
    let mut team = ensemble::default_team_name(None);
    let mut name = None;
    let mut confirm_policy = ConfirmPolicy::Ask;
    let mut print_config = false;
    let mut i = 1;
    while i < launcher_idx {
        match args[i].as_str() {
            "--repo" => {
                repo = take_member_flag_value(args, &mut i, "--repo")?;
            }
            "--team" => {
                let raw = take_member_flag_value(args, &mut i, "--team")?;
                team = ensemble::default_team_name(Some(&raw));
            }
            "--name" => {
                name = Some(take_member_flag_value(args, &mut i, "--name")?);
            }
            "--member" => {
                name = Some(take_member_flag_value(args, &mut i, "--member")?);
            }
            "--confirm-policy" => {
                let raw = take_member_flag_value(args, &mut i, "--confirm-policy")?;
                confirm_policy = ConfirmPolicy::parse(&raw)?;
            }
            "--print-config" => {
                print_config = true;
                i += 1;
            }
            "--timeout" | "--prompt" | "--print-prompt" | "--json" => {
                return Err(format!(
                    "{} is only valid before `agy`, not before `{}`",
                    args[i], args[launcher_idx]
                ));
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            other => {
                return Err(format!(
                    "unexpected positional argument `{other}`; put vendor args after `--`"
                ));
            }
        }
    }
    let vendor_args = args[launcher_idx + 1..].to_vec();
    reject_old_member_tail(&args[launcher_idx], &vendor_args)?;

    Ok(MemberLauncherArgs {
        repo,
        team,
        name,
        vendor_args,
        confirm_policy,
        print_config,
    })
}

fn take_member_flag_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*i + 1)
        .filter(|v| !v.starts_with("--"))
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))?;
    *i += 2;
    Ok(value)
}

fn build_member_launch_plan(
    client: ensemble::mcp_install::ClientKind,
    parsed: MemberLauncherArgs,
    env: &MemberLauncherEnv,
) -> Result<MemberLaunchPlan, String> {
    let repo = absolutize_from(std::path::PathBuf::from(parsed.repo), &env.cwd);
    let exe = absolutize_from(env.exe.clone(), &env.cwd);
    if !repo.is_absolute() {
        return Err(format!(
            "--repo did not resolve to an absolute path ({})",
            repo.display()
        ));
    }
    if !exe.is_absolute() {
        return Err(format!(
            "ensemble binary did not resolve to an absolute path ({})",
            exe.display()
        ));
    }
    let team = ensemble::default_team_name(Some(&parsed.team));
    let member = parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            ensemble::mcp_install::default_member_name(client, env.raw_host.as_deref())
        });
    let session = ensemble::resolve_team_session(
        &repo,
        Some(&team),
        client.as_str(),
        Some(&member),
        env.raw_host.as_deref(),
    );
    let crew = repo.join("crew.toml");
    let params = ensemble::mcp_install::InstallParams {
        exe,
        repo: repo.clone(),
        team: team.clone(),
        name: member.clone(),
        crew: crew.clone(),
    };
    member_confirmation_args(client, parsed.confirm_policy)?;
    let config_env = ensemble::mcp_install::Env {
        home: env.home.clone(),
        codex_home: env.codex_home.clone(),
    };
    let config_path = ensemble::mcp_install::config_path(client, &repo, &config_env);
    if !config_path.is_absolute() {
        return Err(format!(
            "could not determine a config location for `{}`; pass a real home/CODEX_HOME to the process",
            client.as_str()
        ));
    }

    Ok(MemberLaunchPlan {
        client,
        repo,
        team,
        member,
        crew,
        config_path,
        session,
        params,
        vendor_program: env
            .vendor_bin
            .clone()
            .unwrap_or_else(|| client.as_str().to_string()),
        vendor_args: parsed.vendor_args,
        confirm_policy: parsed.confirm_policy,
        print_config: parsed.print_config,
    })
}

fn member_launcher_cmd(client: ensemble::mcp_install::ClientKind, parsed: MemberLauncherArgs) {
    let mut env = runtime_member_launcher_env().unwrap_or_else(|e| {
        eprintln!("ensemble {}: {e}", client.as_str());
        std::process::exit(2);
    });
    env.vendor_bin = vendor_bin_override(client);
    let plan = build_member_launch_plan(client, parsed, &env).unwrap_or_else(|e| {
        eprintln!("ensemble {}: {e}", client.as_str());
        std::process::exit(2);
    });
    let merged = render_member_mcp_config(&plan).unwrap_or_else(|e| {
        eprintln!("ensemble {}: {e}", client.as_str());
        std::process::exit(1);
    });
    if plan.print_config {
        print!("{}", render_member_launch_preview(&plan, &merged));
        return;
    }
    write_member_mcp_config(&plan, &merged).unwrap_or_else(|e| {
        eprintln!("ensemble {}: {e}", client.as_str());
        std::process::exit(1);
    });
    if let Err(e) = std::fs::create_dir_all(&plan.session.root) {
        eprintln!(
            "ensemble {}: create team root {}: {e}",
            client.as_str(),
            plan.session.root.display()
        );
        std::process::exit(1);
    }
    print_member_launch_banner(&plan);
    let argv = build_member_vendor_argv(&plan);
    let config = ensemble::ControlledPtyConfig::new(
        plan.client.as_str(),
        plan.repo.clone(),
        plan.member.clone(),
        plan.repo.clone(),
        plan.vendor_program.clone(),
        argv,
    )
    .env("ENSEMBLE_REPO", plan.repo.as_os_str())
    .env("ENSEMBLE_TEAM", plan.team.as_str())
    .env("ENSEMBLE_MEMBER", plan.member.as_str())
    .env("ENSEMBLE_BOARD", plan.session.board.as_os_str());
    let exit_code = ensemble::run_controlled_pty(config).unwrap_or_else(|e| {
        eprintln!(
            "ensemble {}: start `{}`: {e}",
            client.as_str(),
            plan.vendor_program
        );
        std::process::exit(127);
    });
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn runtime_member_launcher_env() -> Result<MemberLauncherEnv, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?;
    let exe = std::env::current_exe()
        .map(|p| absolutize_from(p, &cwd))
        .map_err(|e| format!("cannot determine this binary's path ({e})"))?;
    Ok(MemberLauncherEnv {
        cwd,
        exe,
        raw_host: raw_hostname(),
        home: home_dir(),
        codex_home: env_path("CODEX_HOME"),
        vendor_bin: None,
    })
}

fn render_member_mcp_config(plan: &MemberLaunchPlan) -> Result<String, String> {
    let existing = match std::fs::read_to_string(&plan.config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {e}", plan.config_path.display())),
    };
    ensemble::mcp_install::render_merged(plan.client, &existing, &plan.params)
        .map_err(|e| format!("render MCP config {}: {e}", plan.config_path.display()))
}

fn write_member_mcp_config(plan: &MemberLaunchPlan, merged: &str) -> Result<(), String> {
    let target = resolve_replace_target(&plan.config_path);
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    write_config(&dir, &target, merged.as_bytes())
}

fn render_member_launch_preview(plan: &MemberLaunchPlan, merged: &str) -> String {
    let vendor_args =
        serde_json::to_string(&build_member_vendor_argv(plan)).unwrap_or_else(|_| "[]".to_string());
    let mcp_args =
        serde_json::to_string(&plan.params.server_args()).unwrap_or_else(|_| "[]".to_string());
    format!(
        "client={}\nmember={}\nteam={}\nconfirm_policy={}\nrepo={}\ncrew={}\nboard={}\nconfig={}\nlaunch={} {}\nmcp_args={}\n\n# merged MCP config preview\n{}",
        plan.client.as_str(),
        plan.member,
        plan.team,
        plan.confirm_policy.as_str(),
        plan.repo.display(),
        plan.crew.display(),
        plan.session.board.display(),
        plan.config_path.display(),
        plan.vendor_program,
        vendor_args,
        mcp_args,
        merged
    )
}

fn print_member_launch_banner(plan: &MemberLaunchPlan) {
    println!(
        "ensemble: launching controlled `{}` as `{}`",
        plan.client.as_str(),
        plan.member
    );
    println!("  repo: {}", plan.repo.display());
    println!("  crew: {}", plan.crew.display());
    println!("  team: {}", plan.team);
    println!("  confirm-policy: {}", plan.confirm_policy.as_str());
    println!("  board: {}", plan.session.board.display());
    println!(
        "  tools: ensemble team inbox --repo {} --team {}",
        plan.repo.display(),
        plan.team
    );
    println!(
        "  steer: ensemble steer {} \"<prompt>\" --repo {}",
        plan.member,
        plan.repo.display()
    );
    println!(
        "  abort: ensemble abort {} --repo {}",
        plan.member,
        plan.repo.display()
    );
    println!(
        "  hard-abort: ensemble abort {} --hard --repo {}",
        plan.member,
        plan.repo.display()
    );
}

fn build_member_vendor_argv(plan: &MemberLaunchPlan) -> Vec<String> {
    let mut args = member_confirmation_args(plan.client, plan.confirm_policy).unwrap_or_default();
    args.extend(plan.vendor_args.iter().cloned());
    args
}

const DEFAULT_AGY_TIMEOUT_SECS: u64 = 180;
const AGY_CONTEXT_LIMIT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgyCliArgs {
    repo: String,
    team: String,
    name: Option<String>,
    timeout_secs: u64,
    confirm_policy: ConfirmPolicy,
    prompt: Option<String>,
    vendor_args: Vec<String>,
    print_prompt: bool,
    json: bool,
}

#[derive(Debug, Clone)]
struct AgyRunPlan {
    session: ensemble::TeamSession,
    timeout_secs: u64,
    confirm_policy: ConfirmPolicy,
    prompt: Option<String>,
    vendor_args: Vec<String>,
    print_prompt: bool,
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgyLaunchMode {
    Interactive,
    TeamTurn,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AgyTurnReport {
    ok: bool,
    member: String,
    team: String,
    cursor: usize,
    text: Option<String>,
    error_kind: Option<String>,
}

#[cfg(test)]
fn parse_agy_args(args: &[String]) -> Result<AgyCliArgs, String> {
    let (launcher_idx, kind) = find_launcher_token(args)?
        .ok_or_else(|| "expected agy after ensemble options".to_string())?;
    match kind {
        LauncherKind::Agy => parse_agy_args_at(args, launcher_idx),
        LauncherKind::Member(client) => Err(format!("expected agy; got {}", client.as_str())),
    }
}

fn parse_agy_args_at(args: &[String], launcher_idx: usize) -> Result<AgyCliArgs, String> {
    let mut repo = ".".to_string();
    let mut team = ensemble::default_team_name(None);
    let mut name = None;
    let mut timeout_secs = DEFAULT_AGY_TIMEOUT_SECS;
    let mut confirm_policy = ConfirmPolicy::Ask;
    let mut print_prompt = false;
    let mut json = false;
    let mut i = 1;
    while i < launcher_idx {
        match args[i].as_str() {
            "--repo" => {
                repo = take_member_flag_value(args, &mut i, "--repo")?;
            }
            "--team" => {
                let raw = take_member_flag_value(args, &mut i, "--team")?;
                team = ensemble::default_team_name(Some(&raw));
            }
            "--name" => {
                name = Some(take_member_flag_value(args, &mut i, "--name")?);
            }
            "--member" => {
                name = Some(take_member_flag_value(args, &mut i, "--member")?);
            }
            "--timeout" => {
                let raw = take_member_flag_value(args, &mut i, "--timeout")?;
                timeout_secs = raw.parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| {
                    "--timeout must be a positive integer number of seconds".to_string()
                })?;
            }
            "--confirm-policy" => {
                let raw = take_member_flag_value(args, &mut i, "--confirm-policy")?;
                confirm_policy = ConfirmPolicy::parse(&raw)?;
            }
            "--prompt" => {
                return Err(
                    "put agy prompt flags after `agy`, for example: ensemble --repo . agy --prompt \"...\""
                        .to_string(),
                );
            }
            "--print-prompt" => {
                print_prompt = true;
                i += 1;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "--print-config" => {
                return Err(
                    "--print-config is only valid before codex, claude, or opencode".to_string(),
                );
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            other => {
                return Err(format!(
                    "unexpected positional argument `{other}` before `agy`; put agy prompt/vendor args after `agy`"
                ));
            }
        }
    }
    let (prompt, vendor_args) = split_agy_prompt_args(&args[launcher_idx + 1..])?;
    reject_old_member_tail(&args[launcher_idx], &vendor_args)?;
    Ok(AgyCliArgs {
        repo,
        team,
        name,
        timeout_secs,
        confirm_policy,
        prompt,
        vendor_args,
        print_prompt,
        json,
    })
}

fn split_agy_prompt_args(raw: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut prompt = None;
    let mut vendor_args = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let arg = &raw[i];
        let inline = arg
            .strip_prefix("--prompt=")
            .or_else(|| arg.strip_prefix("--print="));
        if let Some(value) = inline {
            set_single_agy_prompt(&mut prompt, value.to_string())?;
            i += 1;
            continue;
        }
        if matches!(arg.as_str(), "--prompt" | "--print" | "-p") {
            let value = raw
                .get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{arg} requires a prompt value"))?;
            set_single_agy_prompt(&mut prompt, value)?;
            i += 2;
            continue;
        }
        vendor_args.push(arg.clone());
        i += 1;
    }
    Ok((prompt, vendor_args))
}

fn set_single_agy_prompt(slot: &mut Option<String>, value: String) -> Result<(), String> {
    if slot.is_some() {
        return Err("agy prompt was provided more than once".to_string());
    }
    *slot = Some(value);
    Ok(())
}

fn build_agy_plan(parsed: AgyCliArgs, cwd: &std::path::Path, raw_host: Option<&str>) -> AgyRunPlan {
    let repo = absolutize_from(std::path::PathBuf::from(parsed.repo), cwd);
    let team = ensemble::default_team_name(Some(&parsed.team));
    let member = parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| ensemble::default_member_name("agy", raw_host));
    AgyRunPlan {
        session: ensemble::resolve_team_session(&repo, Some(&team), "agy", Some(&member), raw_host),
        timeout_secs: parsed.timeout_secs,
        confirm_policy: parsed.confirm_policy,
        prompt: parsed.prompt,
        vendor_args: parsed.vendor_args,
        print_prompt: parsed.print_prompt,
        json: parsed.json,
    }
}

fn build_agy_team_prompt(
    session: &ensemble::TeamSession,
    messages: &[ensemble::Message],
    requested: Option<&str>,
    confirm_policy: ConfirmPolicy,
) -> String {
    let mut out = format!(
        "You are `{}` participating in the local ensemble team `{}`.\n\
         Repo: {}\n\n\
         Recent team board, oldest to newest:\n",
        session.member,
        session.team,
        session.repo.display()
    );
    let start = messages.len().saturating_sub(AGY_CONTEXT_LIMIT);
    if messages[start..].is_empty() {
        out.push_str("- (no messages yet)\n");
    } else {
        for m in &messages[start..] {
            out.push_str(&format!("- {} [{}]: {}\n", m.from, m.kind, m.body));
        }
    }
    let requested = requested
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Post a short hello/status update to the team board.");
    out.push_str(&format!(
        "\nRequested turn:\n{requested}\n\n\
         Confirmation policy: {}.\n\
         {}\n\
         Reply with one concise team-board message.\n",
        confirm_policy.as_str(),
        agy_confirmation_instruction(confirm_policy),
    ));
    out
}

fn agy_launch_mode(plan: &AgyRunPlan) -> AgyLaunchMode {
    if plan.prompt.is_none() && !plan.print_prompt && !plan.json {
        AgyLaunchMode::Interactive
    } else {
        AgyLaunchMode::TeamTurn
    }
}

fn build_agy_interactive_argv(plan: &AgyRunPlan) -> Vec<String> {
    let mut args = Vec::new();
    if plan.confirm_policy == ConfirmPolicy::Approve {
        args.push("--dangerously-skip-permissions".to_string());
    }
    args.extend(plan.vendor_args.iter().cloned());
    args
}

fn print_agy_interactive_banner(plan: &AgyRunPlan, argv: &[String]) {
    let vendor_args = serde_json::to_string(argv).unwrap_or_else(|_| "[]".to_string());
    println!(
        "ensemble: launching controlled `agy` as `{}`",
        plan.session.member
    );
    println!("  repo: {}", plan.session.repo.display());
    println!("  team: {}", plan.session.team);
    println!("  confirm-policy: {}", plan.confirm_policy.as_str());
    println!("  board: {}", plan.session.board.display());
    println!(
        "  tools: ensemble team inbox --repo {} --team {}",
        plan.session.repo.display(),
        plan.session.team
    );
    println!(
        "  steer: ensemble steer {} \"<prompt>\" --repo {}",
        plan.session.member,
        plan.session.repo.display()
    );
    println!(
        "  abort: ensemble abort {} --repo {}",
        plan.session.member,
        plan.session.repo.display()
    );
    println!(
        "  hard-abort: ensemble abort {} --hard --repo {}",
        plan.session.member,
        plan.session.repo.display()
    );
    println!("  launch: agy {vendor_args}");
}

fn run_agy_interactive(plan: &AgyRunPlan) -> i32 {
    if let Err(e) = std::fs::create_dir_all(&plan.session.root) {
        eprintln!(
            "ensemble agy: create team root {}: {e}",
            plan.session.root.display()
        );
        return 1;
    }
    let argv = build_agy_interactive_argv(plan);
    print_agy_interactive_banner(plan, &argv);
    let config = ensemble::ControlledPtyConfig::new(
        "agy",
        plan.session.repo.clone(),
        plan.session.member.clone(),
        plan.session.repo.clone(),
        "agy",
        argv,
    )
    .env("ENSEMBLE_REPO", &plan.session.repo)
    .env("ENSEMBLE_TEAM", &plan.session.team)
    .env("ENSEMBLE_MEMBER", &plan.session.member)
    .env("ENSEMBLE_BOARD", &plan.session.board);
    match ensemble::run_controlled_pty(config) {
        Ok(code) => code,
        Err(e) if e.to_string().contains("not found") => {
            eprintln!("ensemble agy: `agy` not found on PATH");
            127
        }
        Err(e) => {
            eprintln!("ensemble agy: start `agy`: {e}");
            1
        }
    }
}

fn agy_confirmation_instruction(policy: ConfirmPolicy) -> &'static str {
    match policy {
        ConfirmPolicy::Ask => {
            "If the CLI shows any interactive option, selector, confirmation, or deny/approve \
             choice that you cannot complete in this non-interactive turn, state the needed decision \
             in text instead of waiting in a chooser."
        }
        ConfirmPolicy::Approve => {
            "Approve tool/permission choices needed to complete this turn when the CLI supports \
             non-interactive approval; if a selector cannot be operated, report the required choice \
             instead of waiting."
        }
        ConfirmPolicy::Deny => {
            "do not approve tool/permission choices. Decline or deny them when possible; if a selector \
             cannot be operated, report the blocked choice instead of waiting."
        }
    }
}

fn run_agy_team_turn(
    session: &ensemble::TeamSession,
    adapter: &dyn Adapter,
    prompt: &str,
) -> Result<(AgyTurnReport, i32), String> {
    match adapter.run(prompt, &session.repo) {
        Ok(out) => {
            let cursor = ensemble::post_team_message(session, &session.member, "result", &out.text)
                .map_err(|e| format!("post agy result: {e}"))?;
            Ok((
                AgyTurnReport {
                    ok: true,
                    member: session.member.clone(),
                    team: session.team.clone(),
                    cursor,
                    text: Some(out.text),
                    error_kind: None,
                },
                0,
            ))
        }
        Err(e) => {
            let kind = adapter_error_kind(&e).to_string();
            let body = format!("agy flaked: {e}");
            let cursor = ensemble::post_team_message(session, &session.member, "flake", &body)
                .map_err(|post| format!("post agy flake after {kind}: {post}"))?;
            let exit = e.exit_code();
            Ok((
                AgyTurnReport {
                    ok: false,
                    member: session.member.clone(),
                    team: session.team.clone(),
                    cursor,
                    text: None,
                    error_kind: Some(kind),
                },
                exit,
            ))
        }
    }
}

fn adapter_error_kind(e: &ensemble::AdapterError) -> &'static str {
    match e {
        ensemble::AdapterError::Flaked(_) => "Flaked",
        ensemble::AdapterError::Empty => "Empty",
        ensemble::AdapterError::RateLimited(_) => "RateLimited",
        ensemble::AdapterError::NotInstalled(_) => "NotInstalled",
    }
}

fn agy_cmd(parsed: AgyCliArgs) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let plan = build_agy_plan(parsed, &cwd, raw_hostname().as_deref());
    if agy_launch_mode(&plan) == AgyLaunchMode::Interactive {
        let exit_code = run_agy_interactive(&plan);
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return;
    }
    let inbox = ensemble::read_team_inbox(&plan.session, 0).unwrap_or_else(|e| {
        eprintln!("ensemble agy: read team inbox: {e}");
        std::process::exit(1);
    });
    let prompt = build_agy_team_prompt(
        &plan.session,
        &inbox.messages,
        plan.prompt.as_deref(),
        plan.confirm_policy,
    );
    if plan.print_prompt {
        print!("{prompt}");
        return;
    }
    let adapter =
        ensemble::AgyAdapter::with_timeout(std::time::Duration::from_secs(plan.timeout_secs))
            .with_dangerously_skip_permissions(plan.confirm_policy == ConfirmPolicy::Approve)
            .with_vendor_args(plan.vendor_args.clone());
    let (report, exit_code) =
        run_agy_team_turn(&plan.session, &adapter, &prompt).unwrap_or_else(|e| {
            eprintln!("ensemble agy: {e}");
            std::process::exit(1);
        });
    if plan.json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else if report.ok {
        println!(
            "agy posted team message as `{}` next={}",
            report.member, report.cursor
        );
    } else {
        eprintln!(
            "agy flaked as `{}` kind={} next={}",
            report.member,
            report.error_kind.as_deref().unwrap_or("Unknown"),
            report.cursor
        );
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

/// Run ONE council target's prompt as a read-only turn → `(label, Ok(reply) | Err(msg))`. A remote
/// target drives the agent on its peer over HTTP; a local one execs the CLI here. A flake is reported,
/// never fatal — the council shows who answered and who couldn't.
fn council_run_one(
    t: &ensemble::CouncilTarget,
    prompt: &str,
    cwd: &Path,
) -> (String, Result<String, String>) {
    let adapter: Box<dyn Adapter> = match &t.node {
        Some(url) => Box::new(RemoteAdapter::new(&t.agent, url)),
        None => match t.agent.as_str() {
            "codex" => Box::new(ExecAdapter::codex()),
            "claude" => Box::new(ExecAdapter::claude()),
            "opencode" => Box::new(ExecAdapter::opencode()),
            "agy" => Box::new(AgyAdapter::new()),
            other => {
                return (
                    t.label.clone(),
                    Err(format!("no local adapter for '{other}'")),
                )
            }
        },
    };
    match adapter.run(prompt, cwd) {
        Ok(out) => (t.label.clone(), Ok(out.text)),
        Err(e) => (t.label.clone(), Err(e.to_string())),
    }
}

/// `ensemble crew inspect [--crew <p>] [--json]` — print parsed crew/gate/reviewer metadata so
/// acceptance scripts can verify governance with the same TOML parser the conductor uses.
fn crew_cmd(args: &[String]) {
    match args.get(2).map(|s| s.as_str()) {
        Some("inspect") => {
            require_value_if_present(args, "--crew");
            let crew = load_crew(args);
            let inspection = crew.inspect();
            if has_flag(args, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&inspection)
                        .expect("crew inspection should serialize")
                );
            } else {
                print!("{}", render_crew_inspection(&inspection));
            }
        }
        _ => {
            eprintln!("usage: ensemble crew inspect [--crew <crew.toml>] [--json]");
            std::process::exit(2);
        }
    }
}

fn render_crew_inspection(i: &CrewInspection) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "pipeline={}\nmin_approvals={}\nmax_rounds={}\non_flake={}\n",
        i.pipeline.join(","),
        i.min_approvals,
        i.max_rounds,
        i.on_flake
    ));
    out.push_str(&format!(
        "test={}\n",
        i.test_command.as_deref().unwrap_or("(none)")
    ));
    if let Some(imp) = &i.implementer {
        out.push_str(&format!("implementer={}\n", imp.agent));
    } else {
        out.push_str("implementer=(missing)\n");
    }
    out.push_str(&format!("reviewers={}\n", i.reviewer_agents.join(",")));
    out.push_str(&format!(
        "distinct_reviewer_agents={}\n",
        i.distinct_reviewer_agents
    ));
    if !i.explicit_remote_agents.is_empty() {
        out.push_str(&format!(
            "explicit_remote_agents={}\n",
            i.explicit_remote_agents.join(",")
        ));
    } else {
        out.push_str("explicit_remote_agents=(none)\n");
    }
    out
}

/// `ensemble run "<task>" [--crew <p>] [--repo <p>]` — a single task, isolated in its own worktree.
fn run_single(args: &[String]) {
    require_value_if_present(args, "--into"); // used by --merge; reject a value-less `--into`
    require_value_if_present(args, "--watch"); // S1a live stream name; reject a value-less `--watch`
    require_value_if_present(args, "--team"); // optional team board mirror for Phase-2 fleet runs
    let task = match positional_tasks(args) {
        tasks if tasks.len() == 1 => tasks[0].clone(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let crew = load_crew(args);
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let watch_name = parse_flag(args, "--watch");
    let team = parse_flag(args, "--team");
    let registry = adapters_for(&crew, !has_flag(args, "--no-discover"));
    let mut c = Conductor::new(crew, registry).with_abort(abort_flag());
    let mut observers: Vec<Box<dyn ensemble::RunObserver>> = Vec::new();
    // S1a/S1b live supervision: --watch <name> opens the stream feed (.ensemble/stream/<name>.ndjson) so
    // the operator can `ensemble watch <name> --follow` the run live, AND the control feed
    // (.ensemble/control/<name>.ndjson) so `ensemble steer/abort <name>` can redirect or stop it.
    if let Some(name) = watch_name.as_deref() {
        eprintln!(
            "ensemble run: live `{name}` — watch: ensemble watch {name} --follow | \
             steer: ensemble steer {name} \"...\" | abort: ensemble abort {name} [--hard]"
        );
        let stream = ensemble::Feed::open(ensemble::member_stream_path(Path::new(&repo), name));
        observers.push(Box::new(ensemble::FeedObserver::new(stream)));
        // S1b control watcher: feed the ControlState from the control feed. Start the cursor at the END
        // so stale commands from a previous run are ignored. Daemon thread — ends when the process exits.
        let ctrl = std::sync::Arc::new(ensemble::ControlState::default());
        let ctrl_w = ctrl.clone();
        let control_feed =
            ensemble::Feed::open(ensemble::member_control_path(Path::new(&repo), name));
        std::thread::spawn(move || {
            let mut cursor = control_feed.len().unwrap_or(0);
            loop {
                ensemble::drain_control(&control_feed, &mut cursor, &ctrl_w);
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        c = c.with_control(ctrl);
    }
    if let Some(team) = team.as_deref() {
        let member = watch_name.as_deref().unwrap_or("run");
        let session =
            ensemble::resolve_team_session(Path::new(&repo), Some(team), "run", Some(member), None);
        observers.push(Box::new(ensemble::TeamObserver::new(session)));
    }
    if !observers.is_empty() {
        c = c.with_stream(Box::new(observers));
    }
    let out = c.run_in_repo(&task, Path::new(&repo));
    print_transcript(&out);
    match out.decision {
        Decision::Landed => {
            print!("LANDED after {} round(s)", out.rounds);
            if let Some(b) = &out.branch {
                if has_flag(args, "--merge") {
                    // Auto-land the kept branch onto --into (default main). A conflict here is a SOFT
                    // failure — the work is safe on `b`; report it (the operator can resolve, or run
                    // the suggested `ensemble merge`). The run itself still LANDED (exit 0).
                    let into = parse_flag(args, "--into").unwrap_or_else(|| "main".to_string());
                    // carry a non-default target into the retry hint so it merges onto the same branch
                    let into_arg = if into == "main" {
                        String::new()
                    } else {
                        format!(" --into {into}")
                    };
                    match ensemble::merge_branch(Path::new(&repo), b, &into) {
                        Ok(ensemble::MergeOutcome::Landed) => print!(" → merged into `{into}`"),
                        Ok(ensemble::MergeOutcome::Conflict(paths)) => print!(
                            " → kept on `{b}`; auto-merge into `{into}` CONFLICTED on [{}] — \
                             resolve, or: ensemble merge {b}{into_arg} --resolver <agent>",
                            paths.join(", ")
                        ),
                        Err(e) => print!(" → kept on `{b}`; auto-merge into `{into}` failed: {e}"),
                    }
                } else {
                    print!(" → work kept on branch `{b}` (land it with: ensemble merge {b})");
                }
            }
            println!();
        }
        Decision::Escalated(why) => {
            eprintln!("ESCALATED after {} round(s): {}", out.rounds, why);
            std::process::exit(1);
        }
    }
}

/// Print the blackboard transcript (every inter-agent message) so the operator can see what each
/// agent actually said — essential for understanding a LANDED or ESCALATED outcome.
fn print_transcript(out: &RunOutcome) {
    println!("─── transcript ───");
    for m in out.blackboard.read_since(0) {
        println!("[{} · {}]\n{}\n", m.from, m.kind, m.body.trim());
    }
    println!("──────────────────");
}

/// `ensemble run-many "<t1>" "<t2>" ... [--crew <p>] [--repo <p>]` — parallel tasks, each in its
/// own worktree. Prints a per-task summary; exits non-zero if any task escalated.
fn run_many(args: &[String]) {
    let tasks = positional_tasks(args);
    if tasks.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let crew = load_crew(args);
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let registry = adapters_for(&crew, !has_flag(args, "--no-discover"));
    let c = Conductor::new(crew, registry).with_abort(abort_flag());
    let outs = c.run_many(&tasks, Path::new(&repo));
    let mut any_escalated = false;
    for (task, out) in tasks.iter().zip(outs.iter()) {
        match &out.decision {
            Decision::Landed => {
                print!("LANDED ({} round(s)): {task}", out.rounds);
                if let Some(b) = &out.branch {
                    print!(" → work kept on branch `{b}` (merge it with: git merge {b})");
                }
                println!();
            }
            Decision::Escalated(why) => {
                any_escalated = true;
                println!("ESCALATED ({} round(s)): {task} — {why}", out.rounds);
            }
        }
    }
    if any_escalated {
        std::process::exit(1);
    }
}

/// Seconds since the Unix epoch (the ledger's timestamps; tests inject their own).
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `ensemble dispatch "<t1>" ... --ledger <db> [--crew <p>] [--repo <p>]` — a DURABLE, RESUMABLE
/// batch: tasks are recorded in a SQLite ledger, claimed at-most-once, and given a terminal record.
/// Re-running resumes (done tasks are skipped; a prior worker's stale claims are recovered).
fn dispatch_cmd(args: &[String]) {
    let tasks = positional_tasks(args);
    if tasks.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let ledger_path = parse_flag(args, "--ledger").unwrap_or_else(|| "ensemble-ledger.db".into());
    let crew = load_crew(args);
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let c = Conductor::new(
        crew.clone(),
        adapters_for(&crew, !has_flag(args, "--no-discover")),
    )
    .with_abort(abort_flag());
    let mut ledger = ensemble::ledger::Ledger::open(Path::new(&ledger_path)).unwrap_or_else(|e| {
        eprintln!("ledger {ledger_path}: {e}");
        std::process::exit(1);
    });
    let worker = format!("worker-{}", std::process::id());
    // recover claims stale > 5 min (a previous worker that died mid-task) before draining; the clock
    // is read FRESH per claim/terminal write so a long batch's late claims aren't seen as stale.
    let stale_before = now_secs() - 300;
    let counts = ensemble::dispatch::run(
        &mut ledger,
        &c,
        &tasks,
        Path::new(&repo),
        &worker,
        &now_secs,
        stale_before,
    )
    .unwrap_or_else(|e| {
        eprintln!("dispatch: {e}");
        std::process::exit(1);
    });
    println!(
        "dispatch: {} done, {} failed, {} queued, {} claimed",
        counts.done, counts.failed, counts.queued, counts.claimed
    );
    if counts.failed > 0 {
        std::process::exit(1);
    }
}

/// `ensemble ledger <status|recover> --ledger <db> [--stale-secs N]` — inspect or recover the ledger.
fn ledger_cmd(args: &[String]) {
    let sub = args.get(2).map(|s| s.as_str());
    let ledger_path = parse_flag(args, "--ledger").unwrap_or_else(|| "ensemble-ledger.db".into());
    let l = ensemble::ledger::Ledger::open(Path::new(&ledger_path)).unwrap_or_else(|e| {
        eprintln!("ledger {ledger_path}: {e}");
        std::process::exit(1);
    });
    match sub {
        Some("status") => {
            let c = l.counts().unwrap_or_default();
            println!(
                "queued={} claimed={} done={} failed={}",
                c.queued, c.claimed, c.done, c.failed
            );
            for t in l.list().unwrap_or_default() {
                let out = t.outcome.clone().unwrap_or_default();
                let suffix = if out.is_empty() {
                    String::new()
                } else {
                    format!(" ({out})")
                };
                println!("  [{}] {} — {}{}", t.state_str(), t.id, t.descr, suffix);
            }
        }
        Some("recover") => {
            let stale = parse_flag(args, "--stale-secs")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(300);
            let n = l.recover_orphans(now_secs() - stale).unwrap_or(0);
            println!("recovered {n} orphaned claim(s)");
        }
        _ => {
            eprintln!("usage: ensemble ledger <status|recover> --ledger <path> [--stale-secs N]");
            std::process::exit(2);
        }
    }
}

/// Value-less switches: they take NO following value, so the arg parser must advance past them by
/// one (not two) — otherwise `--no-discover "task"` would swallow the task as a phantom flag value.
const BARE_SWITCHES: &[&str] = &["--no-discover", "--json", "--merge", "--hard"];

/// The prompt handed to a `--resolver <agent>` CLI on a merge conflict: it runs in the conflicted
/// (mid-merge) worktree and must edit the listed files to a coherent merged result with NO markers,
/// WITHOUT staging/committing (merge_with_resolver validates + completes the merge itself). Pure.
fn build_resolver_prompt(branch: &str, into: &str, paths: &[String]) -> String {
    let mut p = format!(
        "You are resolving a git MERGE CONFLICT from merging branch `{branch}` into `{into}`.\n\
         These files contain conflict markers (<<<<<<<, =======, >>>>>>>):\n"
    );
    for path in paths {
        p.push_str(&format!("  - {path}\n"));
    }
    p.push_str(
        "\nFor EACH file, edit it into a single correct, coherent merged result that preserves the \
         intent of BOTH sides, and REMOVE every conflict marker. Do NOT run `git add`, `git commit`, \
         or `git merge --continue` — just leave the resolved files on disk. Do not touch other files.\n",
    );
    p
}

/// Collect positional task arguments: everything after the subcommand that is neither a `--flag`
/// nor a value flag's value. Bare switches (e.g. `--no-discover`) consume no value.
fn positional_tasks(args: &[String]) -> Vec<String> {
    let mut tasks = Vec::new();
    let mut i = 2; // skip argv[0] (binary) and argv[1] (subcommand)
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            if BARE_SWITCHES.contains(&a.as_str()) {
                i += 1; // value-less switch
            } else {
                i += 2; // value flag: skip the flag AND its value
            }
        } else {
            tasks.push(a.clone());
            i += 1;
        }
    }
    tasks
}

/// Load the crew config: prefer `--crew <path>` (or `crew.toml`), falling back to the repo example.
fn load_crew(args: &[String]) -> CrewConfig {
    let crew_path = parse_flag(args, "--crew").unwrap_or_else(|| "crew.toml".to_string());
    if Path::new(&crew_path).exists() {
        CrewConfig::from_path(Path::new(&crew_path)).unwrap_or_else(|e| {
            eprintln!("crew config: {e}");
            std::process::exit(1);
        })
    } else {
        CrewConfig::from_path(Path::new("examples/crew.toml")).unwrap_or_else(|e| {
            eprintln!("no crew.toml and examples/crew.toml unreadable: {e}");
            std::process::exit(1);
        })
    }
}

/// Phase-1b adapter registry: all four real CLIs (local). Used by `ensemble serve` to host this
/// node's agents.
fn adapters() -> HashMap<String, Box<dyn Adapter>> {
    let mut adapters: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    adapters.insert("codex".into(), Box::new(ExecAdapter::codex()));
    adapters.insert("claude".into(), Box::new(ExecAdapter::claude()));
    adapters.insert("opencode".into(), Box::new(ExecAdapter::opencode()));
    adapters.insert("agy".into(), Box::new(AgyAdapter::new()));
    adapters
}

/// item 6: apply a crew.toml `[agents.<n>]` `timeout`/`args` to a LOCAL ExecAdapter (codex/claude/
/// opencode). `args` (e.g. `["--model","gpt-5.5"]`) is appended after the vendor's base args; `timeout`
/// overrides the per-command deadline. A remote node ignores these (it configures its own).
fn cfg_exec(base: ExecAdapter, crew: &CrewConfig, agent: &str) -> ExecAdapter {
    let mut a = base;
    if let Some(secs) = crew.timeout_for(agent) {
        a = a.with_timeout(std::time::Duration::from_secs(secs));
    }
    if let Some(extra) = crew.args_for(agent) {
        a = a.with_extra_args(extra.to_vec());
    }
    a
}

/// item 6: apply `[agents.agy] timeout` to a LOCAL AgyAdapter. (agy's argv is built specially with
/// `--print-timeout`, so per-agent `args` injection for agy is a documented later refinement.)
fn cfg_agy(crew: &CrewConfig, agent: &str) -> AgyAdapter {
    match crew.timeout_for(agent) {
        Some(secs) => AgyAdapter::with_timeout(std::time::Duration::from_secs(secs)),
        None => AgyAdapter::new(),
    }
}

/// Every agent whose adapter must exist for this crew: each role's agent PLUS any configured
/// `backup` (a different vendor). Backups must be built too — otherwise the conductor's quota-aware
/// substitution can't reach a configured backup, so a rate-limited agent escalates the whole run
/// even though a backup was set (e.g. `[agents.codex] backup = "agy"` with agy not used as a role).
fn crew_agents_with_backups(crew: &CrewConfig) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for role in crew.roles.values() {
        set.insert(role.agent.clone());
        if let Some(backup) = crew.backup_for(&role.agent) {
            set.insert(backup.to_string());
        }
    }
    set
}

/// Crew-aware registry. For each agent a role references (and each configured backup), resolve in
/// priority order: (1) an explicit `[agents.<n>] node = "http://..."` in crew.toml → RemoteAdapter
/// (always wins); (2) when `discover`, a tailnet peer running `ensemble serve` that hosts the agent →
/// RemoteAdapter; (3) the local `ExecAdapter`/`AgyAdapter` fallback. The tailnet is probed only when
/// `discover` is on AND some needed agent lacks an explicit node. An unknown local agent is skipped
/// (a missing adapter already makes the conductor escalate cleanly).
fn adapters_for(crew: &CrewConfig, discover: bool) -> HashMap<String, Box<dyn Adapter>> {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    let agents = crew_agents_with_backups(crew);
    let needs_discovery = discover && agents.iter().any(|a| crew.node_for(a).is_none());
    let discovered = if needs_discovery {
        ensemble::discovery::discover_agent_hosts(7878)
    } else {
        HashMap::new()
    };
    for agent in &agents {
        let agent = agent.as_str();
        if let Some(node) = crew.node_for(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node))); // explicit wins
        } else if let Some(node) = discovered.get(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node))); // auto-discovered
        } else {
            match agent {
                "codex" => {
                    m.insert(
                        agent.into(),
                        Box::new(cfg_exec(ExecAdapter::codex(), crew, agent)),
                    );
                }
                "claude" => {
                    m.insert(
                        agent.into(),
                        Box::new(cfg_exec(ExecAdapter::claude(), crew, agent)),
                    );
                }
                "opencode" => {
                    m.insert(
                        agent.into(),
                        Box::new(cfg_exec(ExecAdapter::opencode(), crew, agent)),
                    );
                }
                "agy" => {
                    m.insert(agent.into(), Box::new(cfg_agy(crew, agent)));
                }
                _ => { /* unknown local agent: skip — conductor escalates on missing adapter */ }
            }
        }
    }
    m
}

/// Resolve a SINGLE agent to an adapter + a label for the ACTUAL target it resolved to (for
/// `ensemble agent`). Priority: an explicit node (exact `local` forces the local CLI, a full URL is
/// used verbatim, or a bare host becomes `http://<host>:7878`) > a discovered tailnet host (when
/// `discover`) > the local CLI by name (label `"local"`). `None` if nothing resolves. Returning the
/// label keeps the JSON `node` field consistent with the resolution actually performed.
fn resolve_one(
    name: &str,
    explicit_node: Option<&str>,
    discover: bool,
) -> Option<(Box<dyn Adapter>, String)> {
    if let Some(node) = explicit_node {
        if is_agent_local_escape(node) {
            return local_agent_adapter(name).map(|adapter| (adapter, "local".to_string()));
        }
        let url = if node.starts_with("http://") || node.starts_with("https://") {
            node.to_string()
        } else {
            format!("http://{node}:7878")
        };
        return Some((Box::new(RemoteAdapter::new(name, &url)), url));
    }
    if discover {
        if let Some(url) = ensemble::discovery::discover_agent_hosts(7878).get(name) {
            return Some((Box::new(RemoteAdapter::new(name, url)), url.clone()));
        }
    }
    local_agent_adapter(name).map(|adapter| (adapter, "local".to_string()))
}

fn is_agent_local_escape(node: &str) -> bool {
    node == "local"
}

fn local_agent_adapter(name: &str) -> Option<Box<dyn Adapter>> {
    let local: Box<dyn Adapter> = match name {
        "codex" => Box::new(ExecAdapter::codex()),
        "claude" => Box::new(ExecAdapter::claude()),
        "opencode" => Box::new(ExecAdapter::opencode()),
        "agy" => Box::new(AgyAdapter::new()),
        _ => return None,
    };
    Some(local)
}

/// If `flag` is present in `args`, its next token must be a real value (not another `--flag`).
/// Exits with a usage error otherwise — so `--node --json` can't silently consume `--json`.
fn require_value_if_present(args: &[String], flag: &str) {
    if let Some(i) = args.iter().position(|a| a == flag) {
        let ok = args
            .get(i + 1)
            .map(|v| !v.starts_with("--"))
            .unwrap_or(false);
        if !ok {
            eprintln!("{flag} requires a value");
            std::process::exit(2);
        }
    }
}

/// `ensemble agent <name> "<task>" [--node auto|<host>] [--repo <p>] [--json]` — delegate ONE turn
/// to a single CLI (local or, via `--node`/discovery, on another machine; edits land in `--repo`
/// via the existing git-sync). The primitive an interactive conductor (Claude Code/codex) shells
/// out to. `--json` emits a one-line machine-readable result; the exit code encodes the failure kind.
fn agent_cmd(args: &[String]) {
    require_value_if_present(args, "--node");
    require_value_if_present(args, "--repo");
    let pos = positional_tasks(args); // exactly [name, task]
    if pos.len() != 2 {
        eprintln!(
            "ensemble agent needs exactly <name> \"<task>\" (quote a multi-word task)\n{USAGE}"
        );
        std::process::exit(2);
    }
    let name = pos[0].clone();
    let task = pos[1].clone();
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let json = has_flag(args, "--json");
    let node = parse_flag(args, "--node");
    // --node <host|url> = explicit; --node auto (or absent, unless --no-discover) = discover.
    let explicit = node.as_deref().filter(|n| *n != "auto");
    let discover = explicit.is_none() && !has_flag(args, "--no-discover");

    let (adapter, node_label) = match resolve_one(&name, explicit, discover) {
        Some(x) => x,
        None => {
            // No adapter resolved — report the target we ATTEMPTED (same scheme rules as resolve_one).
            let attempted = match &explicit {
                Some(n) if n.starts_with("http://") || n.starts_with("https://") => n.to_string(),
                Some(n) if is_agent_local_escape(n) => "local".to_string(),
                Some(n) => format!("http://{n}:7878"),
                None if discover => "auto".to_string(),
                None => "local".to_string(),
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({"agent": name, "node": attempted, "ok": false,
                        "text": "", "branch": serde_json::Value::Null, "error_kind": "NoAdapter"})
                );
            } else {
                eprintln!("no adapter resolved for agent '{name}'");
            }
            std::process::exit(7);
        }
    };

    match adapter.run(&task, Path::new(&repo)) {
        Ok(out) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"agent": out.agent, "node": node_label, "ok": true,
                        "text": out.text, "branch": serde_json::Value::Null,
                        "error_kind": serde_json::Value::Null})
                );
            } else {
                println!("{}", out.text);
            }
        }
        Err(e) => {
            let kind = match &e {
                AdapterError::Flaked(_) => "Flaked",
                AdapterError::Empty => "Empty",
                AdapterError::RateLimited(_) => "RateLimited",
                AdapterError::NotInstalled(_) => "NotInstalled",
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({"agent": name, "node": node_label, "ok": false,
                        "text": "", "branch": serde_json::Value::Null, "error_kind": kind})
                );
            } else {
                eprintln!("agent '{name}' failed: {e}");
            }
            std::process::exit(e.exit_code());
        }
    }
}

/// `ensemble merge <branch> [--into <target>] [--repo <path>] [--resolver <agent>]` — land a kept
/// branch onto `into` (default main): fast-forward or true-merge. On conflict: without `--resolver`
/// it aborts (worktree restored) and reports the conflicting paths; with `--resolver <agent>` it runs
/// ONE AI-resolver round (that local CLI edits the conflicted files), completing the merge ONLY if
/// provably clean, else restoring + escalating (decision 2). NEVER force/auto-accept. Exit 0 = landed,
/// 3 = conflict (escalated), 1 = error.
fn merge_cmd(args: &[String]) {
    require_value_if_present(args, "--into");
    require_value_if_present(args, "--repo");
    require_value_if_present(args, "--resolver");
    let pos = positional_tasks(args);
    if pos.len() != 1 {
        eprintln!("ensemble merge needs exactly <branch>\n{USAGE}");
        std::process::exit(2);
    }
    let branch = &pos[0];
    let into = parse_flag(args, "--into").unwrap_or_else(|| "main".to_string());
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let repo_path = Path::new(&repo);

    let outcome = if let Some(agent) = parse_flag(args, "--resolver") {
        // The resolver edits the conflicted files IN PLACE, so it must be a LOCAL adapter (no remote
        // discovery). A bad agent name is a clean upfront error, not a conflict-escalation.
        let (adapter, _label) = match resolve_one(&agent, None, false) {
            Some(x) => x,
            None => {
                eprintln!("ensemble merge: no local adapter for resolver agent '{agent}'");
                std::process::exit(2);
            }
        };
        ensemble::merge_with_resolver(repo_path, branch, &into, |rp, paths| {
            let prompt = build_resolver_prompt(branch, &into, paths);
            adapter
                .run(&prompt, rp)
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
    } else {
        ensemble::merge_branch(repo_path, branch, &into)
    };

    match outcome {
        Ok(ensemble::MergeOutcome::Landed) => println!("merged {branch} into {into}"),
        Ok(ensemble::MergeOutcome::Conflict(paths)) => {
            eprintln!(
                "merge conflict: {branch} into {into} NOT landed (escalated). Conflicting paths:"
            );
            for p in &paths {
                eprintln!("  {p}");
            }
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("merge failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Adapts a built `Conductor` into the MCP server's `CrewRunner`, so a live member's `ensemble_run`
/// delegates a governed crew sub-run. The conductor (crew.toml + its adapter registry) is built ONCE
/// at server start; each `ensemble_run` call runs `run_in_repo` in its own throwaway worktree.
struct ConductorRunner {
    conductor: Conductor,
}
impl ensemble::mcp::CrewRunner for ConductorRunner {
    fn run(&self, task: &str, repo: &Path) -> ensemble::mcp::RunSummary {
        let out = self.conductor.run_in_repo(task, repo);
        let rounds = out.rounds;
        let (landed, branch, detail) = match out.decision {
            Decision::Landed => (true, out.branch, String::new()),
            Decision::Escalated(why) => (false, None, why),
        };
        ensemble::mcp::RunSummary {
            landed,
            rounds,
            branch,
            detail,
        }
    }
}

struct McpSupervisorRunner;

impl ensemble::mcp::SupervisorRunner for McpSupervisorRunner {
    fn supervise(
        &self,
        req: ensemble::mcp::SuperviseRequest,
        repo: &Path,
        caller: &str,
    ) -> Result<ensemble::mcp::SuperviseSummary, String> {
        let evidence = ensemble::collect_supervisor_evidence(
            repo,
            req.team.as_deref(),
            &req.name,
            req.since,
            50,
        )
        .map_err(|e| format!("collect evidence: {e}"))?;
        let prompt = ensemble::build_supervisor_prompt(&evidence);
        let (adapter, _label) = resolve_one(&req.agent, None, false)
            .ok_or_else(|| format!("no local adapter for agent '{}'", req.agent))?;
        let raw = adapter
            .run(&prompt, repo)
            .map_err(|e| format!("agent '{}': {e}", req.agent))?
            .text;
        let report = ensemble::parse_supervisor_report(&raw).unwrap_or_else(|e| {
            ensemble::SupervisorReport {
                recommendation: ensemble::SupervisorRecommendation::NeedsHuman,
                reason: format!("unparseable supervisor output: {e}"),
                steer: None,
                critical: false,
            }
        });
        let session =
            ensemble::resolve_team_session(repo, Some(&evidence.team), "mcp", Some(caller), None);
        let body = format!(
            "supervise `{}` via `{}`: {:?} - {}",
            req.name, req.agent, report.recommendation, report.reason
        );
        let board_next = ensemble::post_team_message(&session, caller, "supervise", &body)
            .map_err(|e| format!("post board result: {e}"))?;
        let apply = match (req.apply_steer, req.abort_on_critical) {
            (true, true) => ensemble::SupervisorApply::ApplySteerAndAbortOnCritical,
            (true, false) => ensemble::SupervisorApply::ApplySteer,
            (false, true) => ensemble::SupervisorApply::AbortOnCritical,
            (false, false) => ensemble::SupervisorApply::Advisory,
        };
        let control_next = ensemble::control_action_for_report(&report, apply, caller)
            .as_ref()
            .map(|cmd| append_control_direct(repo, None, None, DEFAULT_SERVE_PORT, &req.name, cmd))
            .transpose()?;
        Ok(ensemble::mcp::SuperviseSummary {
            name: req.name,
            team: evidence.team,
            agent: req.agent,
            recommendation: report.recommendation,
            reason: report.reason,
            steer: report.steer,
            critical: report.critical,
            board_next,
            control_next,
        })
    }
}

/// Build the `ensemble_run` crew-runner for `ensemble mcp`, or `None` if no crew.toml is resolvable —
/// then `ensemble_run` reports itself unavailable, but the server STILL starts so the board / claim /
/// worktree / merge / complete / fail tools work (they need no crew). Uses `--crew <path>` when given,
/// else `<repo>/crew.toml`. `adapters_for(.., false)` disables tailnet DISCOVERY (no probe at startup
/// → fast launch); a crew.toml with an explicit `node = "..."` still resolves to that pinned peer's
/// `RemoteAdapter`. Only AUTO-discovery of sub-run agents from the tailnet is the later refinement.
fn mcp_runner(
    args: &[String],
    repo: &str,
) -> Option<std::sync::Arc<dyn ensemble::mcp::CrewRunner>> {
    let path = parse_flag(args, "--crew")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(repo).join("crew.toml"));
    let crew = CrewConfig::from_path(&path).ok()?;
    let registry = adapters_for(&crew, false);
    Some(std::sync::Arc::new(ConductorRunner {
        conductor: Conductor::new(crew, registry),
    }))
}

/// `ensemble mcp [--repo <path>] [--team <name>] [--name <agent>] [--crew <crew.toml>]` — run a stdio MCP server that
/// exposes the crew-participation API (mesh + board + work-queue + worktree + merge + complete/fail +
/// run), so a LIVE CLI launching it as an MCP server becomes a first-class crew member. Session = the
/// repo; the shared board lives at `<repo>/.ensemble/board.jsonl`, the work-queue at
/// `<repo>/.ensemble/ledger.db`. `ensemble_run` delegates a governed crew sub-run via the runner built
/// by `mcp_runner` (absent crew.toml ⇒ every other tool still works). Blocks on stdin until EOF.
fn mcp_cmd(args: &[String]) {
    if args.get(2).map(|s| s.as_str()) == Some("install") {
        return mcp_install_cmd(args);
    }
    if args.get(2).map(|s| s.as_str()) == Some("uninstall") {
        return mcp_uninstall_cmd(args);
    }
    require_value_if_present(args, "--repo");
    require_value_if_present(args, "--team");
    require_value_if_present(args, "--name");
    require_value_if_present(args, "--crew");
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let team = ensemble::default_team_name(parse_flag(args, "--team").as_deref());
    let name = parse_flag(args, "--name").unwrap_or_else(|| format!("mcp-{}", std::process::id()));
    let runner = mcp_runner(args, &repo);
    let ctx = ensemble::mcp::Ctx {
        repo: std::path::PathBuf::from(repo),
        name,
        team,
        runner,
        supervisor: Some(std::sync::Arc::new(McpSupervisorRunner)),
    };
    if let Err(e) = ensemble::mcp::serve_stdio(ctx) {
        eprintln!("ensemble mcp: {e}");
        std::process::exit(1);
    }
}

fn parse_mcp_client_or_exit(args: &[String], action: &str) -> ensemble::mcp_install::ClientKind {
    let client_str = parse_flag(args, "--client").unwrap_or_else(|| {
        eprintln!("ensemble mcp {action}: --client <claude|codex|opencode> is required");
        std::process::exit(2);
    });
    ensemble::mcp_install::ClientKind::parse(&client_str).unwrap_or_else(|e| {
        eprintln!("ensemble mcp {action}: {e}");
        std::process::exit(2);
    })
}

/// `ensemble mcp install --client <claude|codex|opencode> [--repo <p>] [--team <name>] [--name <id>] [--exe <p>]
/// [--crew <p>] [--config <p>] [--print]` — write the chosen CLI's MCP-server config so it launches
/// `ensemble mcp` and becomes a crew member (no hand-editing per-client formats). Everything
/// environment-specific is DERIVED (exe = this binary, repo = cwd, home from env, `$CODEX_HOME`
/// honored); only the per-client FORMAT lives in `mcp_install`, and `--config`/`--print` override the
/// target/let you preview. The merge is idempotent and preserves the user's other servers + comments.
fn mcp_install_cmd(args: &[String]) {
    for flag in [
        "--client", "--repo", "--team", "--name", "--exe", "--crew", "--config",
    ] {
        require_value_if_present(args, flag);
    }
    let client = parse_mcp_client_or_exit(args, "install");
    // DERIVE every environment/user-specific value (never hardcoded); each is overridable by a flag.
    let repo = absolutize(
        parse_flag(args, "--repo")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            }),
    );
    // exe must be ABSOLUTE — the vendor CLI launches it from its OWN cwd. A current_exe() failure is
    // fatal (a relative fallback would point at nothing); an explicit relative --exe is absolutized.
    let exe = absolutize(match parse_flag(args, "--exe") {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_exe().unwrap_or_else(|e| {
            eprintln!("ensemble mcp install: cannot determine this binary's path ({e}) — pass --exe <path>");
            std::process::exit(2);
        }),
    });
    // Default the member name to `<client>@<host>` so members across the fleet don't collide with zero
    // coordination (and it's STABLE across restarts, which the ledger's claim-ownership relies on). An
    // explicit `--name` still wins. Pure derivation lives in `mcp_install`; we just supply the raw host.
    let name = parse_flag(args, "--name").unwrap_or_else(|| {
        ensemble::mcp_install::default_member_name(client, raw_hostname().as_deref())
    });
    let team = ensemble::default_team_name(parse_flag(args, "--team").as_deref());
    // crew must be ABSOLUTE for the same reason (else `--crew crew.toml` resolves against the vendor
    // CLI's cwd at runtime and silently loses the crew runner).
    let crew = absolutize(
        parse_flag(args, "--crew")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| repo.join("crew.toml")),
    );
    let params = ensemble::mcp_install::InstallParams {
        exe,
        repo: repo.clone(),
        team,
        name,
        crew,
    };
    // The exe/repo/crew baked into the generated config MUST be absolute — the vendor CLI launches
    // `ensemble` from its OWN cwd, so a relative path would resolve against the wrong directory and write
    // a broken MCP entry (invalid output). `absolutize` joins the cwd to make a path absolute, but if
    // `current_dir()` ITSELF fails (a deleted/inaccessible cwd) it can't, and a relative path slips
    // through — including the default `--repo` (`.`) and its derived `--crew` (`./crew.toml`). The codex
    // config-location guard below does NOT catch this (its path is absolute via `$HOME`), so validate
    // here and refuse rather than emit a config the CLI will later mis-resolve.
    for (label, p) in [
        ("--exe", &params.exe),
        ("--repo", &params.repo),
        ("--crew", &params.crew),
    ] {
        if !p.is_absolute() {
            eprintln!(
                "ensemble mcp install: `{label}` did not resolve to an absolute path ({}) — the current \
                 directory is unavailable; pass an absolute {label} <path>",
                p.display()
            );
            std::process::exit(2);
        }
    }
    let env = ensemble::mcp_install::Env {
        home: home_dir(),
        codex_home: env_path("CODEX_HOME"),
    };
    let path = match parse_flag(args, "--config") {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let p = ensemble::mcp_install::config_path(client, &repo, &env);
            // a non-absolute default means we couldn't resolve a real location (e.g. codex with no home
            // and no $CODEX_HOME) — refuse rather than silently write to a relative path.
            if !p.is_absolute() {
                eprintln!(
                    "ensemble mcp install: could not determine a config location for `{}` (no home dir / \
                     $CODEX_HOME?) — pass --config <path>",
                    client.as_str()
                );
                std::process::exit(2);
            }
            p
        }
    };
    // read the existing config: ABSENT ⇒ fresh (empty). Any OTHER read error (permission, a directory,
    // invalid UTF-8) must ABORT — never silently treat it as empty and then OVERWRITE a file we could
    // not read.
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("ensemble mcp install: read {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let merged =
        ensemble::mcp_install::render_merged(client, &existing, &params).unwrap_or_else(|e| {
            eprintln!("ensemble mcp install: {} ({})", e, path.display());
            std::process::exit(1);
        });
    if has_flag(args, "--print") {
        println!("# {} config → {}", client.as_str(), path.display());
        print!("{merged}");
        return;
    }
    // Resolve the REAL file we atomically replace, so the swap goes through any symlink (preserving it)
    // instead of turning the user's link into a regular file. Crucially this also follows a DANGLING
    // symlink (target not yet created — common with dotfile managers) to its destination, rather than
    // destroying the link (config + metadata loss). A brand-new regular path is returned unchanged.
    let target = resolve_replace_target(&path);
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // Ensure the dir we ACTUALLY write into exists. We create the RESOLVED target's dir (not `path`'s),
    // so a dangling symlink whose destination dir doesn't exist yet is handled too; for a normal path
    // the two are identical.
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("ensemble mcp install: create {}: {e}", dir.display());
        std::process::exit(1);
    }
    // Atomic write with cleanup that SURVIVES our error exits: `write_config` owns the temp and only
    // ever RETURNS (never process::exit) while it is live, so on any failure the temp's Drop runs and
    // removes it. (process::exit skips destructors — it would otherwise litter a `.ensemble-mcp-*` copy
    // of the merged config in the user's config dir.) We map the error to a String and exit AFTER the
    // temp is gone.
    if let Err(e) = write_config(&dir, &target, merged.as_bytes()) {
        eprintln!("ensemble mcp install: {e}");
        std::process::exit(1);
    }
    println!(
        "ensemble: registered as MCP server for `{}` (member `{}`) → {}",
        client.as_str(),
        params.name,
        path.display()
    );
    println!(
        "  restart {} to pick it up; the crew's board/queue live under {}/.ensemble/",
        client.as_str(),
        params.repo.display()
    );
}

/// `ensemble mcp uninstall --client <claude|codex|opencode> [--repo <p>] [--config <p>] [--print]`
/// removes only ensemble's MCP entry from the selected client config. Missing files or absent entries are
/// successful no-ops; malformed existing config aborts rather than clobbering unrelated data.
fn mcp_uninstall_cmd(args: &[String]) {
    for flag in ["--client", "--repo", "--config"] {
        require_value_if_present(args, flag);
    }
    let client = parse_mcp_client_or_exit(args, "uninstall");
    let repo = absolutize(
        parse_flag(args, "--repo")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            }),
    );
    let env = ensemble::mcp_install::Env {
        home: home_dir(),
        codex_home: env_path("CODEX_HOME"),
    };
    let path = match parse_flag(args, "--config") {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let p = ensemble::mcp_install::config_path(client, &repo, &env);
            if !p.is_absolute() {
                eprintln!(
                    "ensemble mcp uninstall: could not determine a config location for `{}` (no home dir / \
                     $CODEX_HOME?) — pass --config <path>",
                    client.as_str()
                );
                std::process::exit(2);
            }
            p
        }
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "ensemble: no {} config found at {}; nothing to uninstall",
                client.as_str(),
                path.display()
            );
            return;
        }
        Err(e) => {
            eprintln!("ensemble mcp uninstall: read {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let Some(updated) =
        ensemble::mcp_install::render_removed(client, &existing).unwrap_or_else(|e| {
            eprintln!("ensemble mcp uninstall: {} ({})", e, path.display());
            std::process::exit(1);
        })
    else {
        println!(
            "ensemble: no ensemble MCP entry in {} config {}; nothing to uninstall",
            client.as_str(),
            path.display()
        );
        return;
    };
    if has_flag(args, "--print") {
        println!(
            "# {} config after removing ensemble → {}",
            client.as_str(),
            path.display()
        );
        print!("{updated}");
        return;
    }
    let target = resolve_replace_target(&path);
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("ensemble mcp uninstall: create {}: {e}", dir.display());
        std::process::exit(1);
    }
    if let Err(e) = write_config(&dir, &target, updated.as_bytes()) {
        eprintln!("ensemble mcp uninstall: {e}");
        std::process::exit(1);
    }
    println!(
        "ensemble: removed MCP server entry for `{}` → {}",
        client.as_str(),
        path.display()
    );
}

/// Atomically write `contents` to `target` via a RANDOMLY-named, O_EXCL temp in `dir` (the target's own
/// directory, so the rename stays on one filesystem and a pre-placed/raced file or symlink can never be
/// followed or clobbered — the project-scoped `.mcp.json` / `opencode.json` live in a possibly-shared
/// repo). The temp is created 0600; on Unix the existing config's mode is copied onto it BEFORE the
/// rename so an install neither widens nor narrows it.
///
/// CRUCIAL invariant: this NEVER calls `process::exit` while the temp is live. Every failure RETURNS
/// `Err`, so the `NamedTempFile` (or, on a persist failure, the `PersistError` that owns it) is DROPPED
/// on the way out and the temp is removed. `process::exit` runs no destructors, so an exit here would
/// leave a `.ensemble-mcp-*` file — a full copy of the merged config — behind in the user's config dir.
///
/// Windows permission scope (HONEST): a freshly-created temp inherits its DIRECTORY's ACL — exactly what
/// creating the file from scratch (here, or by the CLI itself) would produce. It does NOT preserve a
/// user's manually tightened, file-specific DACL/owner across the replace (std exposes only the
/// read-only attribute, not the DACL; cloning it needs a platform security API + unsafe for a negligible
/// real-world delta — and these configs hold no secrets, only an exe path + args). So the guarantee is
/// precisely "never widened BEYOND THE DIRECTORY DEFAULT", not "byte-identical to the prior file's ACL".
fn write_config(
    dir: &std::path::Path,
    target: &std::path::Path,
    contents: &[u8],
) -> Result<(), String> {
    use std::io::Write as _;
    let mut tf = tempfile::Builder::new()
        .prefix(".ensemble-mcp-")
        .tempfile_in(dir)
        .map_err(|e| format!("temp file in {}: {e}", dir.display()))?;
    tf.write_all(contents)
        .map_err(|e| format!("write temp: {e}"))?;
    // Carry the existing config's access mode onto the replacement (UNIX only — the meaningful
    // multi-user case). A metadata() Err (brand-new target) ⇒ keep the temp's 0600.
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(target) {
        let _ = std::fs::set_permissions(tf.path(), meta.permissions());
    }
    // persist atomically renames over the target. On failure the returned PersistError OWNS the temp;
    // we keep only its io::Error message and let the PersistError drop here, which removes the temp.
    tf.persist(target)
        .map_err(|e| format!("replace {}: {}", target.display(), e.error))?;
    Ok(())
}

/// A NON-EMPTY environment variable as a path, or `None`. An empty value is treated as UNSET, so it
/// never resolves to a bogus relative path (e.g. an empty `$CODEX_HOME` → `config.toml`).
fn env_path(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

/// This machine's raw host name for the default member name (`<client>@<host>`): the platform env var
/// first (`COMPUTERNAME` on Windows, `HOSTNAME` on Unix — the latter isn't always exported to a
/// non-interactive shell), then the `hostname` command, else `None` (caller falls back to a bare client
/// name). The pure short/sanitize step lives in `mcp_install::default_member_name`.
fn raw_hostname() -> Option<String> {
    fn nonempty(key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|s| !s.trim().is_empty())
    }
    nonempty("COMPUTERNAME")
        .or_else(|| nonempty("HOSTNAME"))
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .filter(|s| !s.trim().is_empty())
        })
}

/// The user's home directory, portably: `%USERPROFILE%` (Windows) or `$HOME` (Unix). Precedence is
/// PLATFORM-CORRECT — `%USERPROFILE%` is authoritative on Windows, `$HOME` on Unix — because the wrong
/// one (e.g. `USERPROFILE` leaking into a WSL/Unix env) would resolve codex's default config to the
/// wrong real home and mutate the wrong `config.toml`. Empty if neither is set/non-empty — then a codex
/// install resolves to a non-absolute path and is refused unless `--config <path>` is passed.
fn home_dir() -> std::path::PathBuf {
    let (first, second) = if cfg!(windows) {
        ("USERPROFILE", "HOME")
    } else {
        ("HOME", "USERPROFILE")
    };
    env_path(first)
        .or_else(|| env_path(second))
        .unwrap_or_default()
}

/// Make `p` absolute WITHOUT touching the filesystem (no canonicalize → no symlink resolution, no
/// Windows `\\?\` prefix, works for a not-yet-created repo): an absolute path is kept; a relative one
/// is joined onto the current dir.
fn absolutize(p: std::path::PathBuf) -> std::path::PathBuf {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return p,
    };
    absolutize_from(p, &cwd)
}

fn absolutize_from(p: std::path::PathBuf, cwd: &std::path::Path) -> std::path::PathBuf {
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

/// The real path `ensemble mcp install` should atomically replace. An EXISTING target (through any
/// symlink chain) `canonicalize`s directly. A path that doesn't fully resolve yet is either a DANGLING
/// symlink — followed one bounded step at a time to its destination, so the link is PRESERVED and its
/// (missing) target file is what we create/replace — or a brand-new regular file, returned as-is. The
/// 40-hop bound defangs a symlink cycle (best-effort return rather than spin).
fn resolve_replace_target(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut cur = path.to_path_buf();
    for _ in 0..40 {
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(&cur) {
                Ok(dst) => {
                    cur = if dst.is_absolute() {
                        dst
                    } else {
                        cur.parent().map(|p| p.join(&dst)).unwrap_or(dst)
                    };
                    // the link may point at a real file (chain ends at an existing target) — resolve it.
                    if let Ok(real) = std::fs::canonicalize(&cur) {
                        return real;
                    }
                }
                Err(_) => return cur,
            },
            // not a symlink (a brand-new regular path / dangling destination) or unreadable → use as-is.
            _ => return cur,
        }
    }
    cur
}

/// `ensemble nodes` — probe the tailnet and print which agent each discovered `serve` node hosts.
fn nodes_cmd(args: &[String]) {
    require_value_if_present(args, "--port");
    let port = parse_discovery_port_or_exit(args);
    let hosts = ensemble::discovery::discover_agent_hosts(port);
    if hosts.is_empty() {
        println!(
            "no ensemble nodes discovered (is `tailscale` installed, MagicDNS on, and are peers running `ensemble serve`?)"
        );
        return;
    }
    println!("discovered agent hosts on the tailnet:");
    let mut entries: Vec<(&String, &String)> = hosts.iter().collect();
    entries.sort();
    for (agent, url) in entries {
        println!("  {agent} -> {url}");
    }
}

/// `ensemble doctor` — print a readiness report for THIS machine (which AI CLIs + tailscale are on
/// PATH, is the cwd a git repo) and exit non-zero if the mesh can't run here, so a script can gate
/// on it (`ensemble doctor && ensemble run ...`).
fn doctor_cmd(_args: &[String]) {
    let st = ensemble::doctor::run_checks();
    println!("ensemble doctor — environment readiness:");
    for t in &st {
        let mark = if t.ok { "ok     " } else { "MISSING" };
        if t.hint.is_empty() {
            println!("  [{mark}] {}", t.name);
        } else {
            println!("  [{mark}] {}  — {}", t.name, t.hint);
        }
    }
    if ensemble::doctor::is_ready(&st) {
        println!("\nready: a crew can run here.");
    } else {
        eprintln!("\nNOT ready: need a git repo in the cwd AND at least one AI CLI on PATH.");
        std::process::exit(1);
    }
}

/// True if `flag` (a bare switch like `--no-discover`) is present in `args`.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn parse_discovery_port(args: &[String]) -> Result<Option<u16>, String> {
    let Some(raw) = parse_flag(args, "--port") else {
        return Ok(None);
    };
    parse_control_port_value(&raw).map(Some)
}

fn parse_control_port_value(raw: &str) -> Result<u16, String> {
    let port: u16 = raw
        .parse()
        .map_err(|_| format!("--port must be an integer from 1 to 65535, got `{raw}`"))?;
    if port == 0 {
        return Err("--port must be an integer from 1 to 65535, got `0`".to_string());
    }
    Ok(port)
}

fn parse_discovery_port_or_exit(args: &[String]) -> u16 {
    match parse_discovery_port(args) {
        Ok(Some(port)) => port,
        Ok(None) => DEFAULT_SERVE_PORT,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

fn parse_control_port_or_exit(args: &[String]) -> u16 {
    parse_discovery_port_or_exit(args)
}

fn reject_bind_and_port(args: &[String]) -> Result<(), String> {
    if has_flag(args, "--bind") && has_flag(args, "--port") {
        Err("--bind and --port are mutually exclusive".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn crew_agents_with_backups_includes_configured_backups() {
        // A backup that is not itself a role (e.g. agy backing codex) must still be in the set so
        // its adapter gets built and quota-aware substitution can actually reach it.
        let crew: CrewConfig = toml::from_str(
            r#"
pipeline = ["implement", "review"]
[gate]
min_approvals = 1
max_rounds = 1
on_flake = "exclude"
[roles.implement]
agent = "codex"
[roles.review]
agent = "claude"
[agents.codex]
backup = "agy"
"#,
        )
        .unwrap();
        let set = crew_agents_with_backups(&crew);
        assert!(set.contains("codex"), "role agent codex must be present");
        assert!(set.contains("claude"), "role agent claude must be present");
        assert!(
            set.contains("agy"),
            "codex's backup agy must be built too, else substitution can't reach it"
        );
    }

    #[test]
    fn bare_switch_does_not_swallow_a_following_task() {
        // --no-discover takes no value, so the task after it must survive (codex gate finding).
        let a = argv(&["ensemble", "run", "--no-discover", "do it"]);
        assert_eq!(positional_tasks(&a), vec!["do it".to_string()]);
    }

    #[test]
    fn bare_switch_works_after_and_between_tasks_with_value_flags() {
        let a = argv(&[
            "ensemble",
            "run-many",
            "t1",
            "--no-discover",
            "t2",
            "--repo",
            ".",
        ]);
        assert_eq!(
            positional_tasks(&a),
            vec!["t1".to_string(), "t2".to_string()]
        );
    }

    #[test]
    fn value_flags_still_consume_their_value() {
        let a = argv(&["ensemble", "run", "--crew", "c.toml", "task", "--repo", "."]);
        assert_eq!(positional_tasks(&a), vec!["task".to_string()]);
    }

    #[test]
    fn run_team_and_watch_flags_do_not_become_tasks() {
        let a = argv(&[
            "ensemble",
            "run",
            "--team",
            "main",
            "--watch",
            "main",
            "phase2 task",
        ]);
        assert_eq!(positional_tasks(&a), vec!["phase2 task".to_string()]);
    }

    #[test]
    fn has_flag_detects_a_switch_anywhere() {
        assert!(has_flag(
            &argv(&["ensemble", "run", "--no-discover", "x"]),
            "--no-discover"
        ));
        assert!(!has_flag(&argv(&["ensemble", "run", "x"]), "--no-discover"));
    }

    #[test]
    fn parse_discovery_port_accepts_valid_port() {
        assert_eq!(
            parse_discovery_port(&argv(&["ensemble", "mesh", "--port", "8788"])).unwrap(),
            Some(8788)
        );
        assert_eq!(
            parse_discovery_port(&argv(&["ensemble", "mesh"])).unwrap(),
            None
        );
    }

    #[test]
    fn parse_discovery_port_rejects_invalid_values() {
        let err = parse_discovery_port(&argv(&["ensemble", "nodes", "--port", "abc"])).unwrap_err();
        assert!(err.contains("--port must be an integer"));

        let err = parse_discovery_port(&argv(&["ensemble", "nodes", "--port", "0"])).unwrap_err();
        assert!(err.contains("1 to 65535"));
    }

    #[test]
    fn reject_bind_and_port_rejects_ambiguous_runtime_bind() {
        let err = reject_bind_and_port(&argv(&[
            "ensemble",
            "up",
            "--bind",
            "100.64.0.1:9999",
            "--port",
            "8788",
        ]))
        .unwrap_err();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn crew_inspection_render_surfaces_phase2_gate_inputs() {
        let crew = CrewConfig::from_toml(
            r#"
pipeline = ["implement", "review", "audit"]
[gate]
min_approvals = 2
max_rounds = 2
on_flake = "exclude"
[test]
command = "cargo test --quiet"
[roles.implement]
agent = "codex"
[roles.review]
agent = "claude"
[roles.audit]
agent = "agy"
[agents.claude]
node = "http://m2:7878"
"#,
        )
        .unwrap();

        let rendered = render_crew_inspection(&crew.inspect());

        assert!(rendered.contains("min_approvals=2"));
        assert!(rendered.contains("test=cargo test --quiet"));
        assert!(rendered.contains("reviewers=claude,agy"));
        assert!(rendered.contains("distinct_reviewer_agents=2"));
        assert!(rendered.contains("explicit_remote_agents=claude"));
    }

    #[test]
    fn merge_is_a_bare_switch_resolver_is_a_value_flag() {
        // `--merge` consumes no value (the task after it survives); `--resolver <agent>` consumes its.
        assert_eq!(
            positional_tasks(&argv(&["ensemble", "run", "--merge", "do it"])),
            vec!["do it".to_string()]
        );
        assert_eq!(
            positional_tasks(&argv(&[
                "ensemble",
                "merge",
                "--resolver",
                "claude",
                "ensemble/x",
            ])),
            vec!["ensemble/x".to_string()]
        );
        // combined: --merge (bare) before a value flag, task survives
        assert_eq!(
            positional_tasks(&argv(&[
                "ensemble", "run", "--merge", "--into", "main", "the task",
            ])),
            vec!["the task".to_string()]
        );
    }

    #[test]
    fn team_say_parser_defaults_to_operator() {
        let parsed = parse_team_cmd_args(&argv(&[
            "ensemble", "team", "say", "hello", "--repo", "r", "--team", "ops",
        ]))
        .unwrap();

        assert_eq!(parsed.repo, "r");
        assert_eq!(parsed.team.as_deref(), Some("ops"));
        assert_eq!(parsed.node, None);
        assert_eq!(
            parsed.action,
            TeamCliAction::Say {
                from: "operator".to_string(),
                message: "hello".to_string(),
            }
        );
    }

    #[test]
    fn team_inbox_parser_accepts_since_and_json() {
        let parsed = parse_team_cmd_args(&argv(&[
            "ensemble", "team", "inbox", "--since", "7", "--json",
        ]))
        .unwrap();

        assert!(parsed.json);
        assert_eq!(parsed.action, TeamCliAction::Inbox { since: 7 });
    }

    #[test]
    fn team_parser_accepts_node_for_remote_control_plane() {
        let parsed = parse_team_cmd_args(&argv(&[
            "ensemble", "team", "status", "--team", "ops", "--repo", "/r", "--node", "macbook",
        ]))
        .unwrap();

        assert_eq!(parsed.repo, "/r");
        assert_eq!(parsed.team.as_deref(), Some("ops"));
        assert_eq!(parsed.node.as_deref(), Some("macbook"));
        assert_eq!(parsed.port, DEFAULT_SERVE_PORT);
    }

    #[test]
    fn team_parser_accepts_port_for_remote_control_plane() {
        let parsed = parse_team_cmd_args(&argv(&[
            "ensemble", "team", "status", "--node", "macbook", "--port", "8788",
        ]))
        .unwrap();

        assert_eq!(parsed.node.as_deref(), Some("macbook"));
        assert_eq!(parsed.port, 8788);
    }

    #[test]
    fn team_parser_accepts_token_for_remote_control_plane() {
        let parsed = parse_team_cmd_args(&argv(&[
            "ensemble", "team", "say", "hello", "--node", "macbook", "--token", "secret",
        ]))
        .unwrap();

        assert_eq!(parsed.node.as_deref(), Some("macbook"));
        assert_eq!(parsed.token.as_deref(), Some("secret"));
    }

    #[test]
    fn team_parser_rejects_missing_flag_values() {
        let err = parse_team_cmd_args(&argv(&["ensemble", "team", "say", "--from"])).unwrap_err();
        assert!(err.contains("--from requires a value"));
    }

    #[test]
    fn team_parser_rejects_bad_since_before_io() {
        let err = parse_team_cmd_args(&argv(&["ensemble", "team", "inbox", "--since", "later"]))
            .unwrap_err();
        assert!(err.contains("--since must be a non-negative integer"));
    }

    #[test]
    fn positional_tasks_skips_node_value_for_control_commands() {
        assert_eq!(
            positional_tasks(&argv(&[
                "ensemble",
                "steer",
                "codex@mac",
                "focus",
                "--node",
                "macbook",
                "--port",
                "8788",
            ])),
            vec!["codex@mac".to_string(), "focus".to_string()]
        );
        assert_eq!(
            positional_tasks(&argv(&[
                "ensemble",
                "abort",
                "codex@mac",
                "--hard",
                "--node",
                "macbook",
                "--port",
                "8788",
            ])),
            vec!["codex@mac".to_string()]
        );
    }

    #[test]
    fn supervise_parser_accepts_agent_since_team_and_mutation_flags() {
        let parsed = parse_supervise_args(&argv(&[
            "ensemble",
            "supervise",
            "team-phase1",
            "--repo",
            "work",
            "--team",
            "ops",
            "--agent",
            "codex",
            "--since",
            "7",
            "--json",
            "--apply-steer",
            "--abort-on-critical",
        ]))
        .unwrap();

        assert_eq!(parsed.name, "team-phase1");
        assert_eq!(parsed.repo, "work");
        assert_eq!(parsed.team.as_deref(), Some("ops"));
        assert_eq!(parsed.agent, "codex");
        assert_eq!(parsed.since, 7);
        assert!(parsed.json);
        assert!(parsed.apply_steer);
        assert!(parsed.abort_on_critical);
        assert_eq!(
            supervise_apply_mode(&parsed),
            ensemble::SupervisorApply::ApplySteerAndAbortOnCritical
        );
    }

    #[test]
    fn supervise_parser_rejects_missing_name_and_bad_since() {
        let missing = parse_supervise_args(&argv(&["ensemble", "supervise"])).unwrap_err();
        assert!(missing.contains("needs <name>"));

        let bad_since =
            parse_supervise_args(&argv(&["ensemble", "supervise", "run", "--since", "later"]))
                .unwrap_err();
        assert!(bad_since.contains("--since must be a non-negative integer"));
    }

    #[test]
    fn launcher_parser_handles_global_options_before_member_and_vendor_args_after() {
        let parsed = match parse_launcher_invocation(&argv(&[
            "ensemble",
            "--repo",
            "work",
            "--team",
            "ops",
            "--member",
            "lead@node-a",
            "--print-config",
            "codex",
            "--model",
            "gpt-5",
            "--repo",
            "vendor-owned",
        ]))
        .unwrap()
        .unwrap()
        {
            LauncherInvocation::Member { client, args } => {
                assert_eq!(client, ensemble::mcp_install::ClientKind::Codex);
                args
            }
            other => panic!("expected member launcher, got {other:?}"),
        };

        assert_eq!(parsed.repo, "work");
        assert_eq!(parsed.team, "ops");
        assert_eq!(parsed.name.as_deref(), Some("lead@node-a"));
        assert!(parsed.print_config);
        assert_eq!(
            parsed.vendor_args,
            vec![
                "--model".to_string(),
                "gpt-5".to_string(),
                "--repo".to_string(),
                "vendor-owned".to_string()
            ]
        );
    }

    #[test]
    fn launcher_parser_accepts_confirmation_policy_before_member() {
        let parsed = match parse_launcher_invocation(&argv(&[
            "ensemble",
            "--confirm-policy",
            "approve",
            "claude",
            "--model",
            "sonnet",
        ]))
        .unwrap()
        .unwrap()
        {
            LauncherInvocation::Member { client, args } => {
                assert_eq!(client, ensemble::mcp_install::ClientKind::Claude);
                args
            }
            other => panic!("expected member launcher, got {other:?}"),
        };

        assert_eq!(parsed.confirm_policy, ConfirmPolicy::Approve);
        assert_eq!(
            parsed.vendor_args,
            vec!["--model".to_string(), "sonnet".to_string()]
        );
    }

    #[test]
    fn launcher_parser_rejects_missing_values_unknown_flags_and_old_separator() {
        let missing =
            parse_launcher_invocation(&argv(&["ensemble", "--repo", "codex"])).unwrap_err();
        assert!(missing.contains("--repo requires a value"));

        let unknown =
            parse_launcher_invocation(&argv(&["ensemble", "--dangerous", "codex"])).unwrap_err();
        assert!(unknown.contains("unknown flag `--dangerous`"));

        let old_separator =
            parse_launcher_invocation(&argv(&["ensemble", "codex", "--", "--model", "gpt-5"]))
                .unwrap_err();
        assert!(old_separator.contains("old `--` separator"));

        let bad_policy =
            parse_launcher_invocation(&argv(&["ensemble", "--confirm-policy", "maybe", "codex"]))
                .unwrap_err();
        assert!(bad_policy.contains("--confirm-policy must be one of"));
    }

    #[test]
    fn member_launcher_plan_resolves_defaults_and_team_scoped_mcp_args() {
        let cwd = test_abs("cwd");
        let env = MemberLauncherEnv {
            cwd: cwd.clone(),
            exe: test_abs("bin").join("ensemble"),
            raw_host: Some("node-a.local".to_string()),
            home: test_abs("home"),
            codex_home: None,
            vendor_bin: None,
        };
        let parsed = parse_member_launcher_args(&argv(&[
            "ensemble", "--repo", "work", "--team", "ops", "codex", "--model", "gpt-5",
        ]))
        .unwrap();

        let plan = build_member_launch_plan(ensemble::mcp_install::ClientKind::Codex, parsed, &env)
            .unwrap();

        assert_eq!(plan.repo, cwd.join("work"));
        assert_eq!(plan.team, "ops");
        assert_eq!(plan.member, "codex@node-a");
        assert_eq!(plan.crew, cwd.join("work").join("crew.toml"));
        assert_eq!(plan.vendor_program, "codex");
        assert_eq!(
            plan.vendor_args,
            vec!["--model".to_string(), "gpt-5".to_string()]
        );
        assert_eq!(plan.confirm_policy, ConfirmPolicy::Ask);
        let rendered = ensemble::mcp_install::render_merged(plan.client, "", &plan.params).unwrap();
        let toml: toml::Value = toml::from_str(&rendered).unwrap();
        let args: Vec<&str> = toml["mcp_servers"]["ensemble"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(
            args.windows(2).any(|w| w == ["--team", "ops"]),
            "MCP args must preserve the launcher team: {args:?}"
        );
    }

    #[test]
    fn vendor_bin_env_key_is_per_client_uppercase() {
        assert_eq!(
            vendor_bin_env_key(ensemble::mcp_install::ClientKind::Codex),
            "ENSEMBLE_CODEX_BIN"
        );
        assert_eq!(
            vendor_bin_env_key(ensemble::mcp_install::ClientKind::Claude),
            "ENSEMBLE_CLAUDE_BIN"
        );
        assert_eq!(
            vendor_bin_env_key(ensemble::mcp_install::ClientKind::Opencode),
            "ENSEMBLE_OPENCODE_BIN"
        );
    }

    #[test]
    fn member_launcher_vendor_bin_override_replaces_program() {
        let cwd = test_abs("cwd");
        let fake = test_abs("fake").join("codex.cmd");
        let env = MemberLauncherEnv {
            cwd: cwd.clone(),
            exe: test_abs("bin").join("ensemble"),
            raw_host: Some("node-a".to_string()),
            home: test_abs("home"),
            codex_home: None,
            vendor_bin: Some(fake.display().to_string()),
        };
        let parsed =
            parse_member_launcher_args(&argv(&["ensemble", "--repo", "work", "codex"])).unwrap();

        let plan = build_member_launch_plan(ensemble::mcp_install::ClientKind::Codex, parsed, &env)
            .unwrap();

        // The override pins the exact program; PATH resolution of bare `codex` is bypassed.
        assert_eq!(plan.vendor_program, fake.display().to_string());
    }

    #[test]
    fn serve_service_config_uses_explicit_exe_bind_and_token() {
        let cwd = test_abs("cwd");
        let default_exe = test_abs("bin").join("ensemble");
        let parsed = build_serve_service_config(
            &argv(&[
                "ensemble",
                "serve",
                "--install-service",
                "--exe",
                "bin/ensemble",
                "--bind",
                "100.64.0.1:7878",
                "--token",
                " secret ",
            ]),
            default_exe,
            &cwd,
        )
        .unwrap();

        assert_eq!(parsed.exe, cwd.join("bin/ensemble"));
        assert_eq!(parsed.bind.as_deref(), Some("100.64.0.1:7878"));
        assert_eq!(parsed.port, None);
        assert_eq!(parsed.token.as_deref(), Some("secret"));
    }

    #[test]
    fn serve_service_config_persists_explicit_port() {
        let cwd = test_abs("cwd");
        let default_exe = test_abs("bin").join("ensemble");
        let parsed = build_serve_service_config(
            &argv(&["ensemble", "serve", "--install-service", "--port", "8788"]),
            default_exe.clone(),
            &cwd,
        )
        .unwrap();

        assert_eq!(parsed.exe, default_exe);
        assert_eq!(parsed.bind, None);
        assert_eq!(parsed.port, Some(8788));
    }

    #[test]
    fn serve_service_config_rejects_bind_plus_port() {
        let cwd = test_abs("cwd");
        let default_exe = test_abs("bin").join("ensemble");
        let err = build_serve_service_config(
            &argv(&[
                "ensemble",
                "serve",
                "--install-service",
                "--bind",
                "100.64.0.1:7878",
                "--port",
                "8788",
            ]),
            default_exe,
            &cwd,
        )
        .unwrap_err();

        assert!(err.contains("--bind and --port are mutually exclusive"));
    }

    #[test]
    fn serve_service_config_uses_default_exe_without_baking_env_token() {
        let cwd = test_abs("cwd");
        let default_exe = test_abs("bin").join("ensemble");
        let parsed = build_serve_service_config(
            &argv(&["ensemble", "serve", "--install-service"]),
            default_exe.clone(),
            &cwd,
        )
        .unwrap();

        assert_eq!(parsed.exe, default_exe);
        assert_eq!(parsed.bind, None);
        assert_eq!(parsed.port, None);
        assert_eq!(
            parsed.token, None,
            "service install should bake only an explicit --token, not the caller's ambient env"
        );
    }

    #[test]
    fn missing_marker_matching_is_case_insensitive_and_specific() {
        assert!(text_contains_missing_marker(
            "Unit ENSEMBLE.service is NOT LOADED.",
            &["ensemble.service"],
            &["not loaded"],
            &[]
        ));
        assert!(!text_contains_missing_marker(
            "Failed to disable unit: Permission denied",
            &["ensemble.service"],
            &["not loaded", "does not exist"],
            &[]
        ));
        assert!(!text_contains_missing_marker(
            "Failed to disable dependency.service: not found",
            &["ensemble.service"],
            &["not found"],
            &[]
        ));
        assert!(text_contains_missing_marker(
            "ERROR: The system cannot find the file specified.",
            &["ensemble-serve"],
            &["does not exist"],
            &["the system cannot find the file specified"]
        ));
    }

    #[test]
    fn member_launcher_blank_name_falls_back_consistently() {
        let cwd = test_abs("cwd");
        let env = MemberLauncherEnv {
            cwd: cwd.clone(),
            exe: test_abs("bin").join("ensemble"),
            raw_host: Some("node-a".to_string()),
            home: test_abs("home"),
            codex_home: None,
            vendor_bin: None,
        };
        let parsed = parse_member_launcher_args(&argv(&[
            "ensemble", "--repo", "work", "--member", "   ", "claude",
        ]))
        .unwrap();

        let plan =
            build_member_launch_plan(ensemble::mcp_install::ClientKind::Claude, parsed, &env)
                .unwrap();

        assert_eq!(plan.member, "claude@node-a");
        assert_eq!(plan.session.member, "claude@node-a");
        assert_eq!(plan.params.name, "claude@node-a");
    }

    #[test]
    fn member_launcher_confirm_policy_adds_vendor_args_for_supported_clients() {
        let cwd = test_abs("cwd");
        let env = MemberLauncherEnv {
            cwd: cwd.clone(),
            exe: test_abs("bin").join("ensemble"),
            raw_host: Some("node-a".to_string()),
            home: test_abs("home"),
            codex_home: None,
            vendor_bin: None,
        };
        let parsed = parse_member_launcher_args(&argv(&[
            "ensemble",
            "--repo",
            "work",
            "--confirm-policy",
            "approve",
            "codex",
            "--model",
            "gpt-5",
        ]))
        .unwrap();

        let plan = build_member_launch_plan(ensemble::mcp_install::ClientKind::Codex, parsed, &env)
            .unwrap();

        assert_eq!(
            build_member_vendor_argv(&plan),
            vec![
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--model".to_string(),
                "gpt-5".to_string()
            ]
        );
    }

    #[test]
    fn member_confirmation_policy_maps_supported_vendor_modes() {
        assert!(member_confirmation_args(
            ensemble::mcp_install::ClientKind::Opencode,
            ConfirmPolicy::Ask
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            member_confirmation_args(
                ensemble::mcp_install::ClientKind::Codex,
                ConfirmPolicy::Deny,
            )
            .unwrap(),
            vec![
                "--sandbox".to_string(),
                "read-only".to_string(),
                "--ask-for-approval".to_string(),
                "never".to_string()
            ]
        );
        assert_eq!(
            member_confirmation_args(
                ensemble::mcp_install::ClientKind::Claude,
                ConfirmPolicy::Approve,
            )
            .unwrap(),
            vec!["--dangerously-skip-permissions".to_string()]
        );
        assert_eq!(
            member_confirmation_args(
                ensemble::mcp_install::ClientKind::Claude,
                ConfirmPolicy::Deny,
            )
            .unwrap(),
            vec!["--permission-mode".to_string(), "dontAsk".to_string()]
        );
    }

    #[test]
    fn member_launcher_confirm_policy_rejects_unsupported_clients() {
        let cwd = test_abs("cwd");
        let env = MemberLauncherEnv {
            cwd,
            exe: test_abs("bin").join("ensemble"),
            raw_host: Some("node-a".to_string()),
            home: test_abs("home"),
            codex_home: None,
            vendor_bin: None,
        };
        let parsed = parse_member_launcher_args(&argv(&[
            "ensemble",
            "--confirm-policy",
            "approve",
            "opencode",
        ]))
        .unwrap();

        let err =
            build_member_launch_plan(ensemble::mcp_install::ClientKind::Opencode, parsed, &env)
                .unwrap_err();

        assert!(err.contains("opencode"));
        assert!(err.contains("--confirm-policy approve"));
    }

    #[test]
    fn agy_parser_accepts_timeout_prompt_and_team_flags() {
        let parsed = parse_agy_args(&argv(&[
            "ensemble",
            "--repo",
            "work",
            "--team",
            "ops",
            "--member",
            "agy@node-a",
            "--timeout",
            "30",
            "--confirm-policy",
            "deny",
            "--json",
            "agy",
            "--prompt",
            "summarize board",
            "--continue",
        ]))
        .unwrap();

        assert_eq!(parsed.repo, "work");
        assert_eq!(parsed.team, "ops");
        assert_eq!(parsed.name.as_deref(), Some("agy@node-a"));
        assert_eq!(parsed.timeout_secs, 30);
        assert_eq!(parsed.confirm_policy, ConfirmPolicy::Deny);
        assert_eq!(parsed.prompt.as_deref(), Some("summarize board"));
        assert_eq!(parsed.vendor_args, vec!["--continue".to_string()]);
        assert!(parsed.json);
    }

    #[test]
    fn agy_parser_requires_prompt_after_agy_launcher() {
        let err =
            parse_agy_args(&argv(&["ensemble", "--prompt", "summarize board", "agy"])).unwrap_err();
        assert!(err.contains("put agy prompt flags after `agy`"));
    }

    #[test]
    fn agy_parser_rejects_bad_timeout_before_io() {
        let err = parse_agy_args(&argv(&["ensemble", "--timeout", "0", "agy"])).unwrap_err();
        assert!(err.contains("--timeout must be a positive integer"));
    }

    #[test]
    fn agy_launch_mode_is_interactive_without_prompt_json_or_print_prompt() {
        let cwd = test_abs("cwd");
        let parsed = parse_agy_args(&argv(&[
            "ensemble",
            "--repo",
            "work",
            "--team",
            "ops",
            "--member",
            "agy@node-a",
            "--confirm-policy",
            "approve",
            "agy",
            "--continue",
        ]))
        .unwrap();
        let plan = build_agy_plan(parsed, &cwd, Some("node-a.local"));

        assert_eq!(agy_launch_mode(&plan), AgyLaunchMode::Interactive);
        assert_eq!(
            build_agy_interactive_argv(&plan),
            vec![
                "--dangerously-skip-permissions".to_string(),
                "--continue".to_string()
            ]
        );
    }

    #[test]
    fn agy_launch_mode_uses_team_turn_for_prompt_json_or_print_prompt() {
        let cwd = test_abs("cwd");
        let with_prompt = build_agy_plan(
            parse_agy_args(&argv(&["ensemble", "agy", "--prompt", "read board"])).unwrap(),
            &cwd,
            Some("node-a.local"),
        );
        assert_eq!(agy_launch_mode(&with_prompt), AgyLaunchMode::TeamTurn);

        let with_json = build_agy_plan(
            parse_agy_args(&argv(&["ensemble", "--json", "agy"])).unwrap(),
            &cwd,
            Some("node-a.local"),
        );
        assert_eq!(agy_launch_mode(&with_json), AgyLaunchMode::TeamTurn);

        let with_print_prompt = build_agy_plan(
            parse_agy_args(&argv(&["ensemble", "--print-prompt", "agy"])).unwrap(),
            &cwd,
            Some("node-a.local"),
        );
        assert_eq!(agy_launch_mode(&with_print_prompt), AgyLaunchMode::TeamTurn);
    }

    #[test]
    fn agy_prompt_includes_recent_team_board_and_noninteractive_instruction() {
        let tmp = tempfile::tempdir().unwrap();
        let session =
            ensemble::resolve_team_session(tmp.path(), Some("ops"), "agy", Some("agy@host"), None);
        let messages = vec![
            ensemble::Message {
                from: "operator".to_string(),
                kind: "note".to_string(),
                body: "stay focused".to_string(),
            },
            ensemble::Message {
                from: "codex@host".to_string(),
                kind: "result".to_string(),
                body: "implemented launcher".to_string(),
            },
        ];

        let prompt = build_agy_team_prompt(
            &session,
            &messages,
            Some("review current state"),
            ConfirmPolicy::Deny,
        );

        assert!(prompt.contains("agy@host"));
        assert!(prompt.contains("ops"));
        assert!(prompt.contains("operator [note]: stay focused"));
        assert!(prompt.contains("codex@host [result]: implemented launcher"));
        assert!(prompt.contains("review current state"));
        assert!(prompt.contains("Confirmation policy: deny"));
        assert!(prompt.contains("do not approve"));
    }

    #[test]
    fn agy_team_turn_posts_success_to_the_board() {
        let tmp = tempfile::tempdir().unwrap();
        let session =
            ensemble::resolve_team_session(tmp.path(), None, "agy", Some("agy@host"), None);
        let adapter = ensemble::MockAdapter::new("agy", vec![Ok("hello from agy".to_string())]);

        let (report, exit_code) = run_agy_team_turn(&session, &adapter, "prompt").unwrap();

        assert_eq!(exit_code, 0);
        assert!(report.ok);
        assert_eq!(report.cursor, 1);
        let inbox = ensemble::read_team_inbox(&session, 0).unwrap();
        assert_eq!(inbox.messages[0].from, "agy@host");
        assert_eq!(inbox.messages[0].kind, "result");
        assert_eq!(inbox.messages[0].body, "hello from agy");
    }

    #[test]
    fn agy_team_turn_posts_a_visible_flake_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let session =
            ensemble::resolve_team_session(tmp.path(), None, "agy", Some("agy@host"), None);
        let adapter = ensemble::MockAdapter::new("agy", vec![Err(ensemble::AdapterError::Empty)]);

        let (report, exit_code) = run_agy_team_turn(&session, &adapter, "prompt").unwrap();

        assert_eq!(exit_code, ensemble::AdapterError::Empty.exit_code());
        assert!(!report.ok);
        assert_eq!(report.error_kind.as_deref(), Some("Empty"));
        let inbox = ensemble::read_team_inbox(&session, 0).unwrap();
        assert_eq!(inbox.messages[0].from, "agy@host");
        assert_eq!(inbox.messages[0].kind, "flake");
        assert!(inbox.messages[0].body.contains("agy flaked"));
    }

    fn test_abs(name: &str) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            std::path::PathBuf::from(format!(r"C:\ensemble-test\{name}"))
        }
        #[cfg(not(windows))]
        {
            std::path::PathBuf::from(format!("/ensemble-test/{name}"))
        }
    }

    #[test]
    fn resolver_prompt_lists_paths_and_forbids_committing() {
        let p = build_resolver_prompt(
            "ensemble/z",
            "main",
            &["src/a.rs".to_string(), "src/b.rs".to_string()],
        );
        assert!(
            p.contains("ensemble/z") && p.contains("main"),
            "names the branch + target"
        );
        assert!(
            p.contains("src/a.rs") && p.contains("src/b.rs"),
            "lists every conflicting path"
        );
        assert!(
            p.contains("REMOVE every conflict marker"),
            "asks to remove markers"
        );
        assert!(
            p.contains("Do NOT run `git add`") && p.contains("git commit"),
            "forbids staging/committing (resolver is edit-only)"
        );
    }

    #[test]
    fn resolve_one_explicit_url_wins() {
        // A full URL is used verbatim → RemoteAdapter named for the agent; label = the URL.
        let (a, label) = resolve_one("codex", Some("http://1.2.3.4:9999"), false).unwrap();
        assert_eq!(a.name(), "codex");
        assert_eq!(label, "http://1.2.3.4:9999");
    }

    #[test]
    fn resolve_one_bare_host_maps_to_default_port_url() {
        // A bare host (no scheme) → http://<host>:7878; must not fall through to a local adapter
        // even though "claude" is a known local name. The label reflects the real target.
        let (a, label) = resolve_one("claude", Some("node-b"), false).unwrap();
        assert_eq!(a.name(), "claude");
        assert_eq!(label, "http://node-b:7878");
        // a bare host that merely starts with "http" is still a bare host (not a URL)
        let (_b, label2) = resolve_one("claude", Some("httpbox"), false).unwrap();
        assert_eq!(label2, "http://httpbox:7878");
    }

    #[test]
    fn resolve_one_explicit_node_local_forces_local_adapter() {
        // `--node local` is the explicit escape hatch for integrations that want to
        // bypass discovery/remote routing and force the local CLI.
        let (a, label) = resolve_one("codex", Some("local"), false).unwrap();
        assert_eq!(a.name(), "codex");
        assert_eq!(label, "local");
    }

    #[test]
    fn resolve_one_agent_local_escape_is_exact() {
        // For `ensemble agent`, only the exact lowercase value `local` is the local
        // escape hatch; every other explicit node value stays an HTTP remote target.
        let (_a, upper) = resolve_one("codex", Some("LOCAL"), false).unwrap();
        assert_eq!(upper, "http://LOCAL:7878");
        let (_b, spaced) = resolve_one("codex", Some(" local"), false).unwrap();
        assert_eq!(spaced, "http:// local:7878");
    }

    #[test]
    fn resolve_one_unknown_name_no_node_no_discover_is_none() {
        // No explicit node, discovery off, and an unknown local name → nothing resolves.
        assert!(resolve_one("nope", None, false).is_none());
    }

    #[test]
    fn resolve_one_known_local_name_without_discover_is_local_adapter() {
        // Each known local name resolves to its local adapter (label "local") with no node/discovery.
        for n in ["codex", "claude", "opencode", "agy"] {
            let (a, label) = resolve_one(n, None, false)
                .unwrap_or_else(|| panic!("{n} should resolve to a local adapter"));
            assert_eq!(a.name(), n);
            assert_eq!(label, "local");
        }
    }

    #[test]
    fn control_node_url_bare_host_maps_to_default_port() {
        assert_eq!(
            control_node_url("macbook", DEFAULT_SERVE_PORT).unwrap(),
            "http://macbook:7878"
        );
    }

    #[test]
    fn control_node_url_bare_host_uses_selected_control_port() {
        assert_eq!(
            control_node_url("macbook", 8788).unwrap(),
            "http://macbook:8788"
        );
    }

    #[test]
    fn control_node_url_explicit_url_is_used_without_trailing_slash() {
        assert_eq!(
            control_node_url("https://node.example:9000/", 8788).unwrap(),
            "https://node.example:9000"
        );
    }

    #[test]
    fn control_node_url_rejects_auto_until_discovery_routing_exists() {
        let err = control_node_url("auto", 8788).unwrap_err();
        assert!(err.contains("--node auto is not supported"));
    }

    #[test]
    fn explicit_node_local_is_the_only_file_backed_escape_hatch() {
        assert_eq!(explicit_control_node("local"), None);
        assert_eq!(explicit_control_node("LOCAL"), None);
        assert_eq!(
            explicit_control_node("localhost").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            explicit_control_node("127.0.0.1").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(explicit_control_node("::1").as_deref(), Some("::1"));
        assert_eq!(explicit_control_node("[::1]").as_deref(), Some("[::1]"));
    }

    #[test]
    fn control_node_url_loopback_hosts_still_use_remote_http() {
        assert_eq!(
            control_node_url("localhost", 8788).unwrap(),
            "http://localhost:8788"
        );
        assert_eq!(
            control_node_url("127.0.0.1", 8788).unwrap(),
            "http://127.0.0.1:8788"
        );
        assert_eq!(control_node_url("::1", 8788).unwrap(), "http://[::1]:8788");
        assert_eq!(
            control_node_url("[::1]", 8788).unwrap(),
            "http://[::1]:8788"
        );
    }

    #[test]
    fn control_node_url_preserves_explicit_host_port_when_control_port_is_set() {
        assert_eq!(
            control_node_url("localhost:9000", 8788).unwrap(),
            "http://localhost:9000"
        );
        assert_eq!(
            control_node_url("[::1]:9000", 8788).unwrap(),
            "http://[::1]:9000"
        );
    }

    #[test]
    fn normalize_control_token_rejects_blank_and_control_chars() {
        assert_eq!(
            normalize_control_token(" secret ".to_string()).as_deref(),
            Some("secret")
        );
        assert_eq!(normalize_control_token("   ".to_string()), None);
        assert_eq!(normalize_control_token("bad\n".to_string()), None);
    }

    #[test]
    fn control_token_prefers_valid_explicit_but_falls_back_from_invalid_explicit_to_env() {
        assert_eq!(
            control_token_from_sources(Some(" cli-secret ".to_string()), Some("env-secret".into()))
                .as_deref(),
            Some("cli-secret")
        );
        assert_eq!(
            control_token_from_sources(Some("   ".to_string()), Some(" env-secret ".into()))
                .as_deref(),
            Some("env-secret")
        );
        assert_eq!(
            control_token_from_sources(Some("bad\n".to_string()), Some("env-secret".into()))
                .as_deref(),
            Some("env-secret")
        );
    }

    #[test]
    fn member_node_routing_uses_member_suffix_without_explicit_node() {
        let routed = route_control_member("claude@macbook", None, Some("node-a"), &[]);

        assert_eq!(routed.member, "claude@macbook");
        assert_eq!(routed.node.as_deref(), Some("macbook"));
    }

    #[test]
    fn member_node_routing_keeps_explicit_node_precedence() {
        let mesh = vec![(
            "http://macbook.tail.ts.net:7878".to_string(),
            vec!["claude".to_string()],
        )];
        let routed = route_control_member("claude@macbook", Some("node-b"), Some("node-a"), &mesh);

        assert_eq!(routed.member, "claude@macbook");
        assert_eq!(routed.node.as_deref(), Some("node-b"));
    }

    #[test]
    fn member_node_routing_accepts_node_local_as_a_local_escape_hatch() {
        let mesh = vec![(
            "http://work.tail.ts.net:7878".to_string(),
            vec!["reviewer".to_string()],
        )];
        let routed = route_control_member("reviewer@work", Some("local"), Some("node-a"), &mesh);

        assert_eq!(routed.member, "reviewer@work");
        assert_eq!(routed.node, None);
    }

    #[test]
    fn member_node_routing_keeps_local_suffix_on_local_plane() {
        let routed = route_control_member("codex@node-a", None, Some("node-a.local"), &[]);

        assert_eq!(routed.member, "codex@node-a");
        assert_eq!(routed.node, None);

        let explicit_local = route_control_member("claude@local", None, Some("anything"), &[]);
        assert_eq!(explicit_local.node, None);
    }

    #[test]
    fn member_node_routing_prefers_discovered_url_for_member_suffix() {
        let mesh = vec![
            (
                "http://other.tail.ts.net:7878".to_string(),
                vec!["codex".to_string()],
            ),
            (
                "http://macbook.tail.ts.net:7878".to_string(),
                vec!["claude".to_string()],
            ),
        ];

        let routed = route_control_member("claude@macbook", None, Some("node-a"), &mesh);

        assert_eq!(routed.member, "claude@macbook");
        assert_eq!(
            routed.node.as_deref(),
            Some("http://macbook.tail.ts.net:7878")
        );
    }

    #[test]
    fn control_node_url_preserves_host_port_nodes() {
        assert_eq!(
            control_node_url("127.0.0.2:60315", 8788).unwrap(),
            "http://127.0.0.2:60315"
        );
        assert_eq!(
            control_node_url("phase2-loopback:60315", 8788).unwrap(),
            "http://phase2-loopback:60315"
        );
    }

    #[test]
    fn append_control_node_local_writes_the_local_feed_for_at_member_names() {
        let tmp = tempfile::tempdir().unwrap();

        let next = append_control_direct(
            tmp.path(),
            Some("local"),
            None,
            DEFAULT_SERVE_PORT,
            "reviewer@work",
            &ensemble::ControlCmd::Abort {
                from: "operator".into(),
                hard: false,
            },
        )
        .unwrap();

        assert_eq!(next, 1);
        let path = ensemble::member_control_path(tmp.path(), "reviewer@work");
        let lines = ensemble::Feed::open(path).read_since(0).unwrap();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn control_plane_with_node_posts_append_control_to_remote() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || {
            let mut req = server.recv().unwrap();
            assert_eq!(req.method(), &tiny_http::Method::Post);
            assert_eq!(req.url(), "/control");
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let parsed: ensemble::wire::ControlPlaneRequest = serde_json::from_str(&body).unwrap();
            match parsed {
                ensemble::wire::ControlPlaneRequest::AppendControl { repo, name, cmd } => {
                    assert_eq!(repo, "remote-repo");
                    assert_eq!(name, "codex@mac");
                    assert_eq!(
                        cmd,
                        ensemble::ControlCmd::Abort {
                            from: "operator".into(),
                            hard: true
                        }
                    );
                }
                other => panic!("expected append_control request, got {other:?}"),
            }
            let resp =
                serde_json::to_string(&ensemble::wire::ControlPlaneResponse::ok_next(42)).unwrap();
            req.respond(tiny_http::Response::from_string(resp)).unwrap();
        });

        let cp = control_plane(Some(&url), Some("secret"), DEFAULT_SERVE_PORT).unwrap();
        let next = cp
            .append_control(
                Path::new("remote-repo"),
                "codex@mac",
                &ensemble::ControlCmd::Abort {
                    from: "operator".into(),
                    hard: true,
                },
            )
            .unwrap();

        assert_eq!(next, 42);
        h.join().unwrap();
    }

    #[test]
    fn control_plane_with_token_sends_the_control_header() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let h = std::thread::spawn(move || {
            let mut req = server.recv().unwrap();
            let token = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("x-ensemble-token"))
                .map(|h| h.value.as_str().to_string());
            assert_eq!(token.as_deref(), Some("secret"));
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).unwrap();
            let _parsed: ensemble::wire::ControlPlaneRequest = serde_json::from_str(&body).unwrap();
            let resp =
                serde_json::to_string(&ensemble::wire::ControlPlaneResponse::ok_next(7)).unwrap();
            req.respond(tiny_http::Response::from_string(resp)).unwrap();
        });

        let cp = control_plane(Some(&url), Some("secret"), DEFAULT_SERVE_PORT).unwrap();
        let next = cp
            .append_control(
                Path::new("remote-repo"),
                "codex@mac",
                &ensemble::ControlCmd::Abort {
                    from: "operator".into(),
                    hard: true,
                },
            )
            .unwrap();

        assert_eq!(next, 7);
        h.join().unwrap();
    }
}
