use ensemble::*;
use std::path::Path;

#[test]
#[ignore = "live: requires a working `codex` CLI on PATH + auth"]
fn codex_exec_adapter_answers() {
    let a = ExecAdapter::codex();
    match a.run("Reply with exactly one word: PONG", Path::new(".")) {
        Ok(out) => {
            assert_eq!(out.agent, "codex");
            assert!(out.text.to_uppercase().contains("PONG"));
        }
        Err(AdapterError::NotInstalled(_)) => eprintln!("codex not installed — skipping assertion"),
        Err(e) => panic!("codex live smoke failed: {e}"),
    }
}
