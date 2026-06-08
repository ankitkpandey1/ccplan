use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn version_prints_package_version() {
    let mut cmd = Command::cargo_bin("ccplan").expect("binary is built by cargo");

    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}
