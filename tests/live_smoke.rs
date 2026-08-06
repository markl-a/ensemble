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
        Err(AdapterError::NotInstalled(_)) => {
            panic!("codex CLI not installed - live smoke must not pass by absence")
        }
        Err(e) => panic!("codex live smoke failed: {e}"),
    }
}

#[test]
#[ignore = "live: requires a working `claude` CLI on PATH + auth"]
fn claude_exec_adapter_answers() {
    let a = ExecAdapter::claude();
    match a.run("Reply with exactly one word: PONG", Path::new(".")) {
        Ok(out) => {
            assert_eq!(out.agent, "claude");
            assert!(out.text.to_uppercase().contains("PONG"));
        }
        Err(AdapterError::NotInstalled(_)) => {
            panic!("claude CLI not installed - live smoke must not pass by absence")
        }
        Err(e) => panic!("claude live smoke failed: {e}"),
    }
}

#[test]
#[ignore = "live: requires a working `opencode` CLI on PATH + auth"]
fn opencode_exec_adapter_answers() {
    let a = ExecAdapter::opencode();
    match a.run("Reply with exactly one word: PONG", Path::new(".")) {
        Ok(out) => {
            assert_eq!(out.agent, "opencode");
            assert!(out.text.to_uppercase().contains("PONG"));
        }
        Err(AdapterError::NotInstalled(_)) => {
            panic!("opencode CLI not installed - live smoke must not pass by absence")
        }
        Err(e) => panic!("opencode live smoke failed: {e}"),
    }
}

#[test]
#[ignore = "live: requires `agy` CLI on PATH + interactive auth"]
fn agy_pty_adapter_answers() {
    let a = AgyAdapter::new();
    match a.run("Reply with exactly one word: PONG", Path::new(".")) {
        Ok(out) => {
            assert_eq!(out.agent, "agy");
            assert!(out.text.to_uppercase().contains("PONG"));
        }
        Err(AdapterError::NotInstalled(_)) => {
            panic!("agy CLI not installed - live smoke must not pass by absence")
        }
        Err(e) => panic!("agy live smoke failed: {e}"),
    }
}
