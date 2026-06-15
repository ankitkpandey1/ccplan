use std::fs;

use assert_cmd::Command;
use assert_fs::TempDir;
use ccplan::cli::Shell;
use ccplan::{model::PlanDate, store::Store};
use predicates::prelude::*;

fn ccplan(temp: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccplan"));
    cmd.env("CCPLAN_ROOT", temp.path());
    cmd
}

#[test]
fn version_prints_package_version() {
    let mut cmd = Command::cargo_bin("ccplan").expect("binary is built by cargo");

    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn completion_shells_display_clap_values() {
    assert_eq!(Shell::Bash.to_string(), "bash");
    assert_eq!(Shell::Zsh.to_string(), "zsh");
    assert_eq!(Shell::Fish.to_string(), "fish");
    assert_eq!(Shell::Powershell.to_string(), "powershell");
}

#[test]
fn completions_emit_real_scripts_for_each_shell() {
    let temp = TempDir::new().unwrap();
    for (shell, needle) in [
        ("bash", "complete -F"),
        ("zsh", "#compdef ccplan"),
        ("fish", "complete -c ccplan"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let assert = ccplan(&temp)
            .args(["completions", shell])
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        assert!(
            stdout.len() > 100,
            "{shell} completions should contain a generated script, got {stdout:?}",
        );
        assert!(
            stdout.contains("ccplan"),
            "{shell} completions should mention the ccplan binary",
        );
        assert!(
            stdout.contains(needle),
            "{shell} completions should contain {needle:?}, got {stdout:?}",
        );
        assert!(
            !stdout.contains("placeholder"),
            "{shell} completions should be real generated output, not a placeholder",
        );
    }
}

#[test]
fn build_script_generates_completion_and_man_artifacts() {
    for (name, path, needle) in [
        ("bash", env!("CCPLAN_COMPLETION_BASH"), "complete -F"),
        ("zsh", env!("CCPLAN_COMPLETION_ZSH"), "#compdef ccplan"),
        ("fish", env!("CCPLAN_COMPLETION_FISH"), "complete -c ccplan"),
        (
            "powershell",
            env!("CCPLAN_COMPLETION_POWERSHELL"),
            "Register-ArgumentCompleter",
        ),
    ] {
        let contents = fs::read_to_string(path).unwrap();
        assert!(
            contents.len() > 100,
            "{name} generated artifact should be non-empty at {path}",
        );
        assert!(
            contents.contains("ccplan"),
            "{name} generated artifact should mention ccplan",
        );
        assert!(
            contents.contains(needle),
            "{name} generated artifact should contain {needle:?}",
        );
    }

    let manpage = fs::read_to_string(env!("CCPLAN_MANPAGE")).unwrap();
    assert!(manpage.len() > 100);
    assert!(manpage.contains(".TH"));
    assert!(manpage.contains("ccplan"));
}

#[test]
fn set_from_stdin_uses_temp_store_root_from_environment() {
    let temp = TempDir::new().unwrap();
    let mut set = ccplan(&temp);

    set.args(["set", "--from", "-"])
        .write_stdin(
            r#"
date = "2026-06-08"
timezone = "Asia/Kolkata"

[[block]]
id = "focus"
title = "Focus time"
start = "11:00"
duration = "30m"
"#,
        )
        .assert()
        .success();

    let mut show = ccplan(&temp);
    show.args(["show", "--date", "2026-06-08", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"id\": \"focus\""));
}

#[test]
fn runtime_binary_commands_use_isolated_store_root() {
    let temp = TempDir::new().unwrap();

    ccplan(&temp)
        .args(["set", "--from", "-"])
        .write_stdin(
            r#"
date = "2099-01-01"
timezone = "Asia/Kolkata"

[[block]]
id = "future-focus"
title = "Future focus"
start = "11:00"
duration = "30m"
"#,
        )
        .assert()
        .success();

    for args in [
        &["show", "--date", "2099-01-01", "--json"][..],
        &["now", "--date", "2099-01-01"][..],
        &["next", "--date", "2099-01-01"][..],
        &["agenda", "--date", "2099-01-01"][..],
        &["apply", "--date", "2099-01-01", "--dry-run"][..],
        &["status"][..],
        &["doctor"][..],
        &["completions", "bash"][..],
    ] {
        ccplan(&temp).args(args).assert().success();
    }

    let date: PlanDate = "2099-01-01".parse().unwrap();
    let store = Store::new(temp.path());
    let rev = store.load_plan(&date).unwrap().unwrap().blocks[0].schedule_rev();
    ccplan(&temp)
        .args([
            "fire",
            "--date",
            "2099-01-01",
            "--id",
            "future-focus",
            "--event",
            "start",
            "--rev",
            rev.as_str(),
            "--at",
            "2099-01-01T05:30:00Z",
        ])
        .assert()
        .success();

    for args in [
        &[
            "add",
            "--id",
            "done-me",
            "--title",
            "Done me",
            "--start",
            "23:00",
            "--duration",
            "10m",
        ][..],
        &["done", "done-me"][..],
        &[
            "add",
            "--id",
            "remove-me",
            "--title",
            "Remove me",
            "--start",
            "23:15",
            "--duration",
            "10m",
        ][..],
        &["rm", "remove-me"][..],
        &[
            "add",
            "--id",
            "skip-me",
            "--title",
            "Skip me",
            "--start",
            "23:30",
            "--duration",
            "10m",
        ][..],
        &["skip", "skip-me"][..],
    ] {
        ccplan(&temp).args(args).assert().success();
    }
}
