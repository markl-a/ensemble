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
    ensemble run \"<task>\" [--crew <crew.toml>] [--repo <path>]\n  \
    ensemble run-many \"<task1>\" \"<task2>\" ... [--crew <crew.toml>] [--repo <path>]\n  \
    ensemble dispatch \"<task1>\" ... --ledger <db> [--crew <crew.toml>] [--repo <path>]   (durable, resumable)\n  \
    ensemble ledger <status|recover> --ledger <db> [--stale-secs N]\n  \
    ensemble nodes   (probe the tailnet for `serve` hosts and the agents they offer)\n  \
    ensemble serve [--bind <addr>]   (default 0.0.0.0:7878 — host this node's local agents)\n\n\
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
        Some("serve") => serve_cmd(&args),
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
    let bind = parse_flag(args, "--bind").unwrap_or_else(|| "0.0.0.0:7878".to_string());
    println!("ensemble serve on {bind}");
    if let Err(e) = ensemble::serve(&bind, adapters()) {
        eprintln!("serve: {e}");
        std::process::exit(1);
    }
}

/// `ensemble run "<task>" [--crew <p>] [--repo <p>]` — a single task, isolated in its own worktree.
fn run_single(args: &[String]) {
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
                print!(" → work kept on branch `{b}` (merge it with: git merge {b})");
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
const BARE_SWITCHES: &[&str] = &["--no-discover"];

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
}
