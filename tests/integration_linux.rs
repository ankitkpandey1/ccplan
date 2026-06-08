#![cfg(target_os = "linux")]

use std::process::Command as StdCommand;

use assert_cmd::Command;
use assert_fs::TempDir;
use predicates::prelude::*;

fn ccplan(temp: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ccplan"));
    command.env("CCPLAN_ROOT", temp.path());
    command
}

#[test]
#[ignore = "requires a real systemd --user manager; run in the dedicated native integration job"]
fn systemd_apply_creates_and_clear_removes_timer() {
    if !systemd_user_available() {
        eprintln!("skipping: systemd --user is not available in this environment");
        return;
    }

    let temp = TempDir::new().unwrap();
    let date = "2099-01-01";
    ccplan(&temp)
        .args(["set", "--from", "-"])
        .write_stdin(format!(
            r#"
date = "{date}"
timezone = "Asia/Kolkata"

[[block]]
id = "native-smoke"
title = "Native smoke"
start = "11:00"
duration = "10m"
"#
        ))
        .assert()
        .success();

    ccplan(&temp)
        .args(["apply", "--date", date])
        .assert()
        .success()
        .stdout(predicate::str::contains("add "));

    let timers = systemctl_list_timers();
    assert!(timers.contains("ccplan-2099-01-01-"));

    ccplan(&temp)
        .args(["clear", "--date", date, "--yes", "--purge"])
        .assert()
        .success();

    let timers = systemctl_list_timers();
    assert!(!timers.contains("ccplan-2099-01-01-"));
}

fn systemd_user_available() -> bool {
    StdCommand::new("systemctl")
        .args(["--user", "is-system-running"])
        .status()
        .is_ok_and(|status| status.success())
}

fn systemctl_list_timers() -> String {
    let output = StdCommand::new("systemctl")
        .args(["--user", "list-timers", "ccplan-*", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}
