use ensemble::*;
use std::collections::HashMap;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // ensemble run "<task>" [--crew <path>]
    if args.get(1).map(|s| s.as_str()) != Some("run") {
        eprintln!("usage: ensemble run \"<task>\" [--crew <crew.toml>]");
        std::process::exit(2);
    }
    let task = match args.get(2) {
        Some(t) if !t.starts_with("--") => t.clone(),
        _ => {
            eprintln!("usage: ensemble run \"<task>\" [--crew <crew.toml>]");
            std::process::exit(2);
        }
    };
    let crew_path = parse_flag(&args, "--crew").unwrap_or_else(|| "crew.toml".to_string());
    let crew = if Path::new(&crew_path).exists() {
        CrewConfig::from_path(Path::new(&crew_path)).unwrap_or_else(|e| {
            eprintln!("crew config: {e}");
            std::process::exit(1);
        })
    } else {
        CrewConfig::from_path(Path::new("examples/crew.toml")).unwrap_or_else(|e| {
            eprintln!("no crew.toml and examples/crew.toml unreadable: {e}");
            std::process::exit(1);
        })
    };

    // Phase-1b adapter registry: all four real CLIs.
    let mut adapters: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    adapters.insert("codex".into(), Box::new(ExecAdapter::codex()));
    adapters.insert("claude".into(), Box::new(ExecAdapter::claude()));
    adapters.insert("opencode".into(), Box::new(ExecAdapter::opencode()));
    adapters.insert("agy".into(), Box::new(AgyAdapter::new()));

    let c = Conductor::new(crew, adapters);
    let out = c.run(&task, Path::new("."));
    match out.decision {
        Decision::Landed => {
            println!("LANDED after {} round(s)", out.rounds);
        }
        Decision::Escalated(why) => {
            eprintln!("ESCALATED after {} round(s): {}", out.rounds, why);
            std::process::exit(1);
        }
    }
}

fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
