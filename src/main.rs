use ensemble::*;
use std::collections::HashMap;
use std::path::Path;

const USAGE: &str = "usage:\n  \
    ensemble run \"<task>\" [--crew <crew.toml>] [--repo <path>]\n  \
    ensemble run-many \"<task1>\" \"<task2>\" ... [--crew <crew.toml>] [--repo <path>]";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(|s| s.as_str());
    match sub {
        Some("run") => run_single(&args),
        Some("run-many") => run_many(&args),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
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
    let c = Conductor::new(crew, adapters());
    let out = c.run_in_repo(&task, Path::new(&repo));
    match out.decision {
        Decision::Landed => println!("LANDED after {} round(s)", out.rounds),
        Decision::Escalated(why) => {
            eprintln!("ESCALATED after {} round(s): {}", out.rounds, why);
            std::process::exit(1);
        }
    }
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
    let c = Conductor::new(crew, adapters());
    let outs = c.run_many(&tasks, Path::new(&repo));
    let mut any_escalated = false;
    for (task, out) in tasks.iter().zip(outs.iter()) {
        match &out.decision {
            Decision::Landed => println!("LANDED ({} round(s)): {task}", out.rounds),
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

/// Collect positional task arguments: everything after the subcommand that is neither a `--flag`
/// nor a flag's value.
fn positional_tasks(args: &[String]) -> Vec<String> {
    let mut tasks = Vec::new();
    let mut i = 2; // skip argv[0] (binary) and argv[1] (subcommand)
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            i += 2; // skip the flag and its value
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

/// Phase-1b adapter registry: all four real CLIs.
fn adapters() -> HashMap<String, Box<dyn Adapter>> {
    let mut adapters: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    adapters.insert("codex".into(), Box::new(ExecAdapter::codex()));
    adapters.insert("claude".into(), Box::new(ExecAdapter::claude()));
    adapters.insert("opencode".into(), Box::new(ExecAdapter::opencode()));
    adapters.insert("agy".into(), Box::new(AgyAdapter::new()));
    adapters
}

fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
