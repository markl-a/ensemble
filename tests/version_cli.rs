use std::process::Command;

fn ensemble_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ensemble")
}

fn assert_prints_version(arg: &str) {
    let out = Command::new(ensemble_bin())
        .arg(arg)
        .output()
        .expect("run ensemble");
    assert!(
        out.status.success(),
        "{arg} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn version_subcommand_and_flags_print_package_version() {
    for arg in ["version", "--version", "-V"] {
        assert_prints_version(arg);
    }
}
