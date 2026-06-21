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

const USAGE: &str = "usage:\n  \
    ensemble run \"<task>\" [--crew <crew.toml>] [--repo <path>] [--merge [--into <target>]]\n  \
    ensemble run-many \"<task1>\" \"<task2>\" ... [--crew <crew.toml>] [--repo <path>]\n  \
    ensemble dispatch \"<task1>\" ... --ledger <db> [--crew <crew.toml>] [--repo <path>]   (durable, resumable)\n  \
    ensemble ledger <status|recover> --ledger <db> [--stale-secs N]\n  \
    ensemble nodes   (probe the tailnet for `serve` hosts and the agents they offer)\n  \
    ensemble mesh   (this node's CLIs + which agents each tailnet peer hosts)\n  \
    ensemble doctor   (check this machine is ready: which AI CLIs + tailscale are on PATH, is-git-repo)\n  \
    ensemble agent <name> \"<task>\" [--node auto|<host>] [--repo <path>] [--json]   (delegate ONE turn to one CLI)\n  \
    ensemble merge <branch> [--into <target>] [--repo <path>] [--resolver <agent>]   (land a kept branch; conflict → escalate, or --resolver runs ONE AI round)\n  \
    ensemble serve [--bind <addr>]   (default: this node's tailnet IP:7878; loopback if no tailnet)\n  \
    ensemble up [--bind <addr>]   (quick start: show the mesh, then serve in the foreground)\n  \
    ensemble mcp [--repo <path>] [--name <agent>] [--crew <crew.toml>]   (stdio MCP server: make a LIVE CLI a crew member — mesh + board + queue + worktree + merge + run)\n  \
    ensemble mcp install --client <claude|codex|opencode> [--repo <p>] [--name <id>] [--exe <p>] [--crew <p>] [--config <p>] [--print]   (one-click: register `ensemble mcp` into that CLI's config)\n\n\
    run/run-many/dispatch auto-discover tailnet `serve` hosts for any agent without an explicit\n  \
    [agents.<n>] node = ... in crew.toml; pass --no-discover to stay local.";

fn main() {
    let args: Vec<String> = std::env::args().collect();
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
        Some("dispatch") => dispatch_cmd(&args),
        Some("ledger") => ledger_cmd(&args),
        Some("nodes") => nodes_cmd(&args),
        Some("mesh") => mesh_cmd(&args),
        Some("doctor") => doctor_cmd(&args),
        Some("agent") => agent_cmd(&args),
        Some("merge") => merge_cmd(&args),
        Some("mcp") => mcp_cmd(&args),
        Some("serve") => serve_cmd(&args),
        Some("up") => up_cmd(&args),
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
    let explicit = parse_flag(args, "--bind");
    // Default to the tailnet interface so serve is reachable only over the tailnet, not the LAN.
    let self_ips = ensemble::discovery::self_tailscale_ips();
    let bind = ensemble::resolve_bind(&self_ips, explicit.as_deref(), 7878);
    if let ensemble::BindAddr::Loopback(_) = bind {
        eprintln!(
            "ensemble: no tailnet IP found (is tailscale up?) — binding loopback only (local). \
             Pass --bind <addr> to override."
        );
    }
    let addr = bind.addr().to_string();
    println!("ensemble serve on {addr}");
    if let Err(e) = ensemble::serve(&addr, adapters()) {
        eprintln!("serve: {e}");
        std::process::exit(1);
    }
}

/// `ensemble mesh` — print which AI CLIs are on THIS node + which agents each discovered tailnet
/// peer hosts. Read-only (no side effects).
fn mesh_cmd(_args: &[String]) {
    let local = ensemble::present_clis();
    let hosts = ensemble::discover_mesh(7878);
    println!("{}", ensemble::render_mesh(&local, &hosts));
}

/// `ensemble up [--bind <addr>]` — the quick-start: resolve the bind (tailnet-only by default),
/// print the mesh (local CLIs + tailnet hosts), then serve in the FOREGROUND until Ctrl-C. The
/// permanent/boot-started path is `serve --install-service` (tick C), not `up`.
fn up_cmd(args: &[String]) {
    let explicit = parse_flag(args, "--bind");
    let self_ips = ensemble::discovery::self_tailscale_ips();
    let bind = ensemble::resolve_bind(&self_ips, explicit.as_deref(), 7878);
    if let ensemble::BindAddr::Loopback(_) = bind {
        eprintln!(
            "ensemble: no tailnet IP found (is tailscale up?) — serving loopback only (local). \
             Pass --bind <addr> to override."
        );
    }
    let addr = bind.addr().to_string();
    let local = ensemble::present_clis();
    let hosts = ensemble::discover_mesh(7878);
    println!("{}", ensemble::render_up(&addr, &local, &hosts));
    // Belt-and-suspenders flush before the long blocking serve. Rust's stdout is line-buffered
    // (LineWriter), so println!'s trailing newline already flushed the banner — this just makes the
    // ordering explicit at a blocking boundary.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    if let Err(e) = ensemble::serve(&addr, adapters()) {
        eprintln!("serve: {e}");
        std::process::exit(1);
    }
}

/// `ensemble run "<task>" [--crew <p>] [--repo <p>]` — a single task, isolated in its own worktree.
fn run_single(args: &[String]) {
    require_value_if_present(args, "--into"); // used by --merge; reject a value-less `--into`
    let task = match positional_tasks(args) {
        tasks if tasks.len() == 1 => tasks[0].clone(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let crew = load_crew(args);
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let registry = adapters_for(&crew, !has_flag(args, "--no-discover"));
    let c = Conductor::new(crew, registry).with_abort(abort_flag());
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
const BARE_SWITCHES: &[&str] = &["--no-discover", "--json", "--merge"];

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

/// Crew-aware registry. For each agent a role references, resolve in priority order: (1) an explicit
/// `[agents.<n>] node = "http://..."` in crew.toml → RemoteAdapter (always wins); (2) when `discover`,
/// a tailnet peer running `ensemble serve` that hosts the agent → RemoteAdapter; (3) the local
/// `ExecAdapter`/`AgyAdapter` fallback. The tailnet is probed only when `discover` is on AND some
/// needed agent lacks an explicit node. An unknown local agent is skipped (a missing adapter already
/// makes the conductor escalate cleanly).
fn adapters_for(crew: &CrewConfig, discover: bool) -> HashMap<String, Box<dyn Adapter>> {
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    let agents: std::collections::HashSet<&str> =
        crew.roles.values().map(|r| r.agent.as_str()).collect();
    let needs_discovery = discover && agents.iter().any(|a| crew.node_for(a).is_none());
    let discovered = if needs_discovery {
        ensemble::discovery::discover_agent_hosts(7878)
    } else {
        HashMap::new()
    };
    for agent in agents {
        if let Some(node) = crew.node_for(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node))); // explicit wins
        } else if let Some(node) = discovered.get(agent) {
            m.insert(agent.into(), Box::new(RemoteAdapter::new(agent, node))); // auto-discovered
        } else {
            match agent {
                "codex" => {
                    m.insert(agent.into(), Box::new(ExecAdapter::codex()));
                }
                "claude" => {
                    m.insert(agent.into(), Box::new(ExecAdapter::claude()));
                }
                "opencode" => {
                    m.insert(agent.into(), Box::new(ExecAdapter::opencode()));
                }
                "agy" => {
                    m.insert(agent.into(), Box::new(AgyAdapter::new()));
                }
                _ => { /* unknown local agent: skip — conductor escalates on missing adapter */ }
            }
        }
    }
    m
}

/// Resolve a SINGLE agent to an adapter + a label for the ACTUAL target it resolved to (for
/// `ensemble agent`). Priority: an explicit node (a full URL used verbatim, or a bare host →
/// `http://<host>:7878`) > a discovered tailnet host (when `discover`) > the local CLI by name
/// (label `"local"`). `None` if nothing resolves. Returning the label keeps the JSON `node` field
/// consistent with the resolution actually performed.
fn resolve_one(
    name: &str,
    explicit_node: Option<&str>,
    discover: bool,
) -> Option<(Box<dyn Adapter>, String)> {
    if let Some(node) = explicit_node {
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
    let local: Box<dyn Adapter> = match name {
        "codex" => Box::new(ExecAdapter::codex()),
        "claude" => Box::new(ExecAdapter::claude()),
        "opencode" => Box::new(ExecAdapter::opencode()),
        "agy" => Box::new(AgyAdapter::new()),
        _ => return None,
    };
    Some((local, "local".to_string()))
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
                AdapterError::RateLimited => "RateLimited",
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
            eprintln!("merge conflict: {branch} into {into} NOT landed (escalated). Conflicting paths:");
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
        ensemble::mcp::RunSummary { landed, rounds, branch, detail }
    }
}

/// Build the `ensemble_run` crew-runner for `ensemble mcp`, or `None` if no crew.toml is resolvable —
/// then `ensemble_run` reports itself unavailable, but the server STILL starts so the board / claim /
/// worktree / merge / complete / fail tools work (they need no crew). Uses `--crew <path>` when given,
/// else `<repo>/crew.toml`. `adapters_for(.., false)` disables tailnet DISCOVERY (no probe at startup
/// → fast launch); a crew.toml with an explicit `node = "..."` still resolves to that pinned peer's
/// `RemoteAdapter`. Only AUTO-discovery of sub-run agents from the tailnet is the later refinement.
fn mcp_runner(args: &[String], repo: &str) -> Option<std::sync::Arc<dyn ensemble::mcp::CrewRunner>> {
    let path = parse_flag(args, "--crew")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(repo).join("crew.toml"));
    let crew = CrewConfig::from_path(&path).ok()?;
    let registry = adapters_for(&crew, false);
    Some(std::sync::Arc::new(ConductorRunner {
        conductor: Conductor::new(crew, registry),
    }))
}

/// `ensemble mcp [--repo <path>] [--name <agent>] [--crew <crew.toml>]` — run a stdio MCP server that
/// exposes the crew-participation API (mesh + board + work-queue + worktree + merge + complete/fail +
/// run), so a LIVE CLI launching it as an MCP server becomes a first-class crew member. Session = the
/// repo; the shared board lives at `<repo>/.ensemble/board.jsonl`, the work-queue at
/// `<repo>/.ensemble/ledger.db`. `ensemble_run` delegates a governed crew sub-run via the runner built
/// by `mcp_runner` (absent crew.toml ⇒ every other tool still works). Blocks on stdin until EOF.
fn mcp_cmd(args: &[String]) {
    if args.get(2).map(|s| s.as_str()) == Some("install") {
        return mcp_install_cmd(args);
    }
    require_value_if_present(args, "--repo");
    require_value_if_present(args, "--name");
    require_value_if_present(args, "--crew");
    let repo = parse_flag(args, "--repo").unwrap_or_else(|| ".".to_string());
    let name = parse_flag(args, "--name").unwrap_or_else(|| format!("mcp-{}", std::process::id()));
    let runner = mcp_runner(args, &repo);
    let ctx = ensemble::mcp::Ctx {
        repo: std::path::PathBuf::from(repo),
        name,
        runner,
    };
    if let Err(e) = ensemble::mcp::serve_stdio(ctx) {
        eprintln!("ensemble mcp: {e}");
        std::process::exit(1);
    }
}

/// `ensemble mcp install --client <claude|codex|opencode> [--repo <p>] [--name <id>] [--exe <p>]
/// [--crew <p>] [--config <p>] [--print]` — write the chosen CLI's MCP-server config so it launches
/// `ensemble mcp` and becomes a crew member (no hand-editing per-client formats). Everything
/// environment-specific is DERIVED (exe = this binary, repo = cwd, home from env, `$CODEX_HOME`
/// honored); only the per-client FORMAT lives in `mcp_install`, and `--config`/`--print` override the
/// target/let you preview. The merge is idempotent and preserves the user's other servers + comments.
fn mcp_install_cmd(args: &[String]) {
    for flag in ["--client", "--repo", "--name", "--exe", "--crew", "--config"] {
        require_value_if_present(args, flag);
    }
    let client_str = parse_flag(args, "--client").unwrap_or_else(|| {
        eprintln!("ensemble mcp install: --client <claude|codex|opencode> is required");
        std::process::exit(2);
    });
    let client = ensemble::mcp_install::ClientKind::parse(&client_str).unwrap_or_else(|e| {
        eprintln!("ensemble mcp install: {e}");
        std::process::exit(2);
    });
    // DERIVE every environment/user-specific value (never hardcoded); each is overridable by a flag.
    let repo = absolutize(
        parse_flag(args, "--repo")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))),
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
    let name = parse_flag(args, "--name").unwrap_or_else(|| client.as_str().to_string());
    // crew must be ABSOLUTE for the same reason (else `--crew crew.toml` resolves against the vendor
    // CLI's cwd at runtime and silently loses the crew runner).
    let crew = absolutize(
        parse_flag(args, "--crew")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| repo.join("crew.toml")),
    );
    let params = ensemble::mcp_install::InstallParams { exe, repo: repo.clone(), name, crew };
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
    let merged = ensemble::mcp_install::render_merged(client, &existing, &params).unwrap_or_else(|e| {
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
    println!("  restart {} to pick it up; the crew's board/queue live under {}/.ensemble/", client.as_str(), params.repo.display());
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
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p)
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
fn nodes_cmd(_args: &[String]) {
    let hosts = ensemble::discovery::discover_agent_hosts(7878);
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

#[cfg(test)]
mod tests {
    use super::*;
    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
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
    fn has_flag_detects_a_switch_anywhere() {
        assert!(has_flag(
            &argv(&["ensemble", "run", "--no-discover", "x"]),
            "--no-discover"
        ));
        assert!(!has_flag(&argv(&["ensemble", "run", "x"]), "--no-discover"));
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
                "ensemble", "merge", "--resolver", "claude", "ensemble/x",
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
    fn resolver_prompt_lists_paths_and_forbids_committing() {
        let p = build_resolver_prompt(
            "ensemble/z",
            "main",
            &["src/a.rs".to_string(), "src/b.rs".to_string()],
        );
        assert!(p.contains("ensemble/z") && p.contains("main"), "names the branch + target");
        assert!(p.contains("src/a.rs") && p.contains("src/b.rs"), "lists every conflicting path");
        assert!(p.contains("REMOVE every conflict marker"), "asks to remove markers");
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
        let (a, label) = resolve_one("claude", Some("ayaneo"), false).unwrap();
        assert_eq!(a.name(), "claude");
        assert_eq!(label, "http://ayaneo:7878");
        // a bare host that merely starts with "http" is still a bare host (not a URL)
        let (_b, label2) = resolve_one("claude", Some("httpbox"), false).unwrap();
        assert_eq!(label2, "http://httpbox:7878");
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
}
