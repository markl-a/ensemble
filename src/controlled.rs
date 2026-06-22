use crate::supervise::{member_control_path, ControlCmd};
use crate::Feed;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_PROMPT_DELAY: Duration = Duration::from_millis(250);
const CONTROL_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyProgram {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ControlledPtyConfig {
    pub label: String,
    pub repo: PathBuf,
    pub control_name: String,
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    pub envs: Vec<(OsString, OsString)>,
    pub prompt_delay: Duration,
    pub relay_output: bool,
}

impl ControlledPtyConfig {
    pub fn new(
        label: impl Into<String>,
        repo: impl Into<PathBuf>,
        control_name: impl Into<String>,
        cwd: impl Into<PathBuf>,
        program: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            label: label.into(),
            repo: repo.into(),
            control_name: control_name.into(),
            cwd: cwd.into(),
            program: program.into(),
            args,
            envs: Vec::new(),
            prompt_delay: DEFAULT_PROMPT_DELAY,
            relay_output: true,
        }
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    pub fn relay_output(mut self, relay_output: bool) -> Self {
        self.relay_output = relay_output;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlScript {
    pub interrupt: bool,
    pub prompt: Option<String>,
    pub kill: bool,
}

pub fn pty_program_for_vendor(program: &str, args: &[String]) -> PtyProgram {
    #[cfg(windows)]
    {
        let mut shell_args = vec!["/C".to_string(), program.to_string()];
        shell_args.extend(args.iter().cloned());
        PtyProgram {
            program: "cmd".to_string(),
            args: shell_args,
        }
    }
    #[cfg(not(windows))]
    {
        PtyProgram {
            program: program.to_string(),
            args: args.to_vec(),
        }
    }
}

pub fn control_script(cmd: &ControlCmd) -> ControlScript {
    match cmd {
        ControlCmd::Steer { prompt, .. } => ControlScript {
            interrupt: true,
            prompt: Some(prompt.clone()),
            kill: false,
        },
        ControlCmd::Abort { hard, .. } => ControlScript {
            interrupt: true,
            prompt: None,
            kill: *hard,
        },
    }
}

pub fn prompt_enter_bytes(prompt: &str) -> Vec<u8> {
    let mut bytes = prompt.as_bytes().to_vec();
    bytes.push(b'\r');
    bytes
}

fn write_script(
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    script: &ControlScript,
    delay: Duration,
) {
    if script.interrupt {
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(b"\x1b");
            let _ = w.flush();
        }
    }
    if let Some(prompt) = &script.prompt {
        std::thread::sleep(delay);
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(&prompt_enter_bytes(prompt));
            let _ = w.flush();
        }
    }
}

pub fn run_controlled_pty(config: ControlledPtyConfig) -> Result<i32, String> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty: {e}"))?;

    let program = pty_program_for_vendor(&config.program, &config.args);
    let mut cmd = CommandBuilder::new(&program.program);
    for arg in &program.args {
        cmd.arg(arg);
    }
    cmd.cwd(&config.cwd);
    for (key, value) in &config.envs {
        cmd.env(key, value);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("start `{}`: {e}", config.program))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone pty reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take pty writer: {e}"))?;
    let writer = Arc::new(Mutex::new(writer));

    let (output_done_tx, output_done_rx) = mpsc::channel();
    let relay_output = config.relay_output;
    let output_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if relay_output {
                        let mut stdout = std::io::stdout().lock();
                        if stdout.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = stdout.flush();
                    }
                }
                Err(_) => break,
            }
        }
        let _ = output_done_tx.send(());
    });

    let input_writer = Arc::clone(&writer);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let Ok(mut w) = input_writer.lock() else {
                        break;
                    };
                    if w.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = w.flush();
                }
                Err(_) => break,
            }
        }
    });

    let hard_abort = Arc::new(AtomicBool::new(false));
    let control_abort = Arc::clone(&hard_abort);
    let control_writer = Arc::clone(&writer);
    let feed = Feed::open(member_control_path(&config.repo, &config.control_name));
    let mut cursor = feed.read_since(0).map(|lines| lines.len()).unwrap_or(0);
    let prompt_delay = config.prompt_delay;
    std::thread::spawn(move || loop {
        std::thread::sleep(CONTROL_POLL);
        let lines = match feed.read_since(cursor) {
            Ok(lines) => lines,
            Err(_) => continue,
        };
        for line in &lines {
            let Ok(cmd) = serde_json::from_str::<ControlCmd>(line) else {
                continue;
            };
            let script = control_script(&cmd);
            write_script(&control_writer, &script, prompt_delay);
            if script.kill {
                control_abort.store(true, Ordering::Relaxed);
            }
        }
        cursor += lines.len();
    });

    let exit = loop {
        if hard_abort.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break 130;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.exit_code() as i32,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait `{}`: {e}", config.program));
            }
        }
    };

    drop(pair.master);
    if output_done_rx.recv_timeout(Duration::from_secs(2)).is_ok() {
        let _ = output_thread.join();
    }
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_program_uses_cmd_on_windows_for_ps1_cmd_shims() {
        let args = vec!["--continue".to_string()];
        let p = pty_program_for_vendor("claude", &args);
        #[cfg(windows)]
        assert_eq!(
            p,
            PtyProgram {
                program: "cmd".to_string(),
                args: vec![
                    "/C".to_string(),
                    "claude".to_string(),
                    "--continue".to_string()
                ]
            }
        );
        #[cfg(not(windows))]
        assert_eq!(
            p,
            PtyProgram {
                program: "claude".to_string(),
                args
            }
        );
    }

    #[test]
    fn steer_control_interrupts_and_injects_prompt_without_killing() {
        let script = control_script(&ControlCmd::Steer {
            from: "codex@host".to_string(),
            prompt: "focus on auth only".to_string(),
        });
        assert_eq!(
            script,
            ControlScript {
                interrupt: true,
                prompt: Some("focus on auth only".to_string()),
                kill: false
            }
        );
        assert_eq!(prompt_enter_bytes("focus"), b"focus\r");
    }

    #[test]
    fn hard_abort_interrupts_and_kills() {
        let script = control_script(&ControlCmd::Abort {
            from: "operator".to_string(),
            hard: true,
        });
        assert_eq!(
            script,
            ControlScript {
                interrupt: true,
                prompt: None,
                kill: true
            }
        );
    }

    #[test]
    fn controlled_pty_returns_child_exit_code() {
        #[cfg(windows)]
        let config = ControlledPtyConfig::new(
            "test",
            std::env::current_dir().unwrap(),
            "test@local",
            std::env::current_dir().unwrap(),
            "cmd",
            vec![
                "/C".to_string(),
                "exit".to_string(),
                "/b".to_string(),
                "7".to_string(),
            ],
        )
        .relay_output(false);
        #[cfg(not(windows))]
        let config = ControlledPtyConfig::new(
            "test",
            std::env::current_dir().unwrap(),
            "test@local",
            std::env::current_dir().unwrap(),
            "sh",
            vec!["-c".to_string(), "exit 7".to_string()],
        )
        .relay_output(false);

        let code = run_controlled_pty(config).unwrap();

        assert_eq!(code, 7);
    }
}
