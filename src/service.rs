use std::path::{Path, PathBuf};

pub const WINDOWS_TASK_NAME: &str = "ensemble-serve";
pub const LAUNCHD_LABEL: &str = "com.ensemble.serve";
pub const SYSTEMD_UNIT_NAME: &str = "ensemble.service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeServiceConfig {
    pub exe: PathBuf,
    pub bind: Option<String>,
    pub token: Option<String>,
}

pub fn serve_program_args(cfg: &ServeServiceConfig) -> Vec<String> {
    let mut args = vec!["serve".to_string()];
    if let Some(bind) = cfg.bind.as_deref().filter(|s| !s.trim().is_empty()) {
        args.push("--bind".to_string());
        args.push(bind.to_string());
    }
    if let Some(token) = cfg.token.as_deref().filter(|s| !s.trim().is_empty()) {
        args.push("--token".to_string());
        args.push(token.to_string());
    }
    args
}

pub fn windows_install_argv(cfg: &ServeServiceConfig) -> Vec<String> {
    vec![
        "/Create".to_string(),
        "/TN".to_string(),
        WINDOWS_TASK_NAME.to_string(),
        "/SC".to_string(),
        "ONLOGON".to_string(),
        "/TR".to_string(),
        windows_service_commandline(cfg),
        "/F".to_string(),
    ]
}

pub fn windows_uninstall_argv() -> Vec<String> {
    vec![
        "/Delete".to_string(),
        "/TN".to_string(),
        WINDOWS_TASK_NAME.to_string(),
        "/F".to_string(),
    ]
}

pub fn windows_run_argv() -> Vec<String> {
    vec![
        "/Run".to_string(),
        "/TN".to_string(),
        WINDOWS_TASK_NAME.to_string(),
    ]
}

pub fn windows_end_argv() -> Vec<String> {
    vec![
        "/End".to_string(),
        "/TN".to_string(),
        WINDOWS_TASK_NAME.to_string(),
    ]
}

pub fn launchd_agent_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

pub fn launchd_plist(cfg: &ServeServiceConfig) -> String {
    let mut args = vec![cfg.exe.display().to_string()];
    args.extend(serve_program_args(cfg));
    let arg_xml = args
        .iter()
        .map(|arg| format!("        <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
{}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/ensemble-serve.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/ensemble-serve.err.log</string>
</dict>
</plist>
"#,
        xml_escape(LAUNCHD_LABEL),
        arg_xml
    )
}

pub fn systemd_user_unit_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(SYSTEMD_UNIT_NAME)
}

pub fn systemd_unit(cfg: &ServeServiceConfig) -> String {
    let mut args = vec![cfg.exe.display().to_string()];
    args.extend(serve_program_args(cfg));
    let exec_start = args
        .iter()
        .map(|arg| systemd_quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\n\
Description=ensemble serve agent host\n\
After=network-online.target\n\n\
[Service]\n\
ExecStart={exec_start}\n\
Restart=on-failure\n\
RestartSec=5s\n\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

fn windows_service_commandline(cfg: &ServeServiceConfig) -> String {
    let mut args = vec![cfg.exe.display().to_string()];
    args.extend(serve_program_args(cfg));
    args.iter()
        .map(|arg| windows_quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn windows_quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn systemd_quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '@' | '=' | '+')
        })
    {
        return arg.to_string();
    }
    let escaped = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(exe: &str) -> ServeServiceConfig {
        ServeServiceConfig {
            exe: PathBuf::from(exe),
            bind: None,
            token: None,
        }
    }

    #[test]
    fn serve_args_omit_bind_by_default_to_inherit_safe_bind() {
        assert_eq!(
            serve_program_args(&cfg("/opt/ensemble/bin/ensemble")),
            vec!["serve"]
        );
    }

    #[test]
    fn serve_args_include_explicit_bind_and_token_when_requested() {
        let cfg = ServeServiceConfig {
            exe: PathBuf::from("/opt/ensemble/bin/ensemble"),
            bind: Some("100.64.0.1:7878".to_string()),
            token: Some("secret".to_string()),
        };

        assert_eq!(
            serve_program_args(&cfg),
            vec!["serve", "--bind", "100.64.0.1:7878", "--token", "secret"]
        );
    }

    #[test]
    fn windows_install_uses_task_scheduler_logon_task() {
        let cfg = ServeServiceConfig {
            exe: PathBuf::from(r"C:\Program Files\ensemble\ensemble.exe"),
            bind: Some("100.64.0.1:7878".to_string()),
            token: None,
        };

        assert_eq!(
            windows_install_argv(&cfg),
            vec![
                "/Create",
                "/TN",
                WINDOWS_TASK_NAME,
                "/SC",
                "ONLOGON",
                "/TR",
                r#""C:\Program Files\ensemble\ensemble.exe" serve --bind 100.64.0.1:7878"#,
                "/F",
            ]
        );
    }

    #[test]
    fn windows_uninstall_deletes_the_known_task() {
        assert_eq!(
            windows_uninstall_argv(),
            vec!["/Delete", "/TN", WINDOWS_TASK_NAME, "/F"]
        );
    }

    #[test]
    fn windows_run_starts_the_known_task_now() {
        assert_eq!(windows_run_argv(), vec!["/Run", "/TN", WINDOWS_TASK_NAME]);
    }

    #[test]
    fn windows_end_stops_the_known_task_now() {
        assert_eq!(windows_end_argv(), vec!["/End", "/TN", WINDOWS_TASK_NAME]);
    }

    #[test]
    fn launchd_plist_uses_program_arguments_not_shell_joining() {
        let cfg = ServeServiceConfig {
            exe: PathBuf::from("/Applications/Ensemble Bin/ensemble"),
            bind: Some("100.64.0.1:7878".to_string()),
            token: None,
        };
        let plist = launchd_plist(&cfg);

        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>com.ensemble.serve</string>"));
        assert!(plist.contains("<string>/Applications/Ensemble Bin/ensemble</string>"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("<string>--bind</string>"));
        assert!(plist.contains("<string>100.64.0.1:7878</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn launchd_path_is_user_agent_path() {
        assert_eq!(
            launchd_agent_path(Path::new("/Users/dev")),
            Path::new("/Users/dev")
                .join("Library")
                .join("LaunchAgents")
                .join("com.ensemble.serve.plist")
        );
    }

    #[test]
    fn systemd_unit_quotes_paths_and_restarts_on_failure() {
        let cfg = ServeServiceConfig {
            exe: PathBuf::from("/home/dev/Ensemble Bin/ensemble"),
            bind: Some("100.64.0.1:7878".to_string()),
            token: None,
        };
        let unit = systemd_unit(&cfg);

        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("Description=ensemble serve agent host"));
        assert!(unit.contains(
            "ExecStart=\"/home/dev/Ensemble Bin/ensemble\" serve --bind 100.64.0.1:7878"
        ));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_unit_escapes_percent_specifiers_and_dollar_expansion() {
        let cfg = ServeServiceConfig {
            exe: PathBuf::from("/home/dev/%bin/$ensemble"),
            bind: None,
            token: Some("abc%H$USER".to_string()),
        };
        let unit = systemd_unit(&cfg);

        assert!(unit
            .contains("ExecStart=\"/home/dev/%%bin/$$ensemble\" serve --token \"abc%%H$$USER\""));
    }

    #[test]
    fn systemd_path_is_user_unit_path() {
        assert_eq!(
            systemd_user_unit_path(Path::new("/home/dev")),
            Path::new("/home/dev")
                .join(".config")
                .join("systemd")
                .join("user")
                .join("ensemble.service")
        );
    }
}
