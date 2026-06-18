use std::{
    fs,
    path::{Path, PathBuf},
};

use toml::Value;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_repo_file(relative: &str) -> String {
    fs::read_to_string(repo_path(relative)).unwrap_or_else(|error| {
        panic!("failed to read {relative}: {error}");
    })
}

fn assert_file_contains(relative: &str, needles: &[&str]) {
    let contents = read_repo_file(relative);
    for needle in needles {
        assert!(
            contents.contains(needle),
            "{relative} should contain {needle:?}",
        );
    }
}

#[test]
fn oss_hygiene_files_are_present_and_specific() {
    assert_file_contains(
        "LICENSE-APACHE",
        &["Apache License", "Version 2.0, January 2004"],
    );
    assert_file_contains(
        "LICENSE-MIT",
        &["MIT License", "Permission is hereby granted"],
    );
    assert_file_contains(
        "CONTRIBUTING.md",
        &[
            "Conventional Commits",
            "cargo fmt --all -- --check",
            "CONVENTIONS.md",
        ],
    );
    assert_file_contains(
        "CODE_OF_CONDUCT.md",
        &["Contributor Covenant Code of Conduct", "Version 2.1"],
    );
    assert_file_contains(
        "SECURITY.md",
        &["run:", "allowed_executables", "private vulnerability"],
    );
    assert_file_contains(
        "CHANGELOG.md",
        &["Keep a Changelog", "## [Unreleased]", "## [1.0.0]"],
    );
    assert_file_contains(
        ".github/PULL_REQUEST_TEMPLATE.md",
        &["Definition of Done", "coverage", "cargo deny check"],
    );
    assert_file_contains(
        ".github/ISSUE_TEMPLATE/bug.yml",
        &["name: Bug report", "ccplan --version"],
    );
    assert_file_contains(
        ".github/ISSUE_TEMPLATE/feature.yml",
        &["name: Feature request", "Non-goals"],
    );
}

#[test]
fn cargo_manifest_declares_release_metadata() {
    let manifest: Value = toml::from_str(&read_repo_file("Cargo.toml")).unwrap();
    let package = &manifest["package"];
    assert_eq!(package["license"].as_str(), Some("MIT OR Apache-2.0"));
    assert_eq!(
        package["homepage"].as_str(),
        Some("https://github.com/ankitkpandey1/ccplan"),
    );
    assert!(package["authors"].as_array().is_some_and(|authors| {
        authors.iter().any(|author| {
            author
                .as_str()
                .is_some_and(|value| value.contains("Ankit Pandey"))
        })
    }));

    let binstall = &package["metadata"]["binstall"];
    assert!(binstall["pkg-url"].as_str().is_some_and(|url| {
        url.contains("github.com/ankitkpandey1/ccplan/releases/download")
            && url.contains("{ archive-format }")
    }));
    assert_eq!(binstall["pkg-fmt"].as_str(), Some("txz"));
    assert!(
        binstall["bin-dir"]
            .as_str()
            .is_some_and(|path| { path.contains("{ bin }") && path.contains("{ target }") })
    );
    assert_eq!(
        binstall["overrides"]["x86_64-pc-windows-msvc"]["pkg-fmt"].as_str(),
        Some("zip"),
    );

    let dist = &manifest["workspace"]["metadata"]["dist"];
    assert_eq!(dist["cargo-dist-version"].as_str(), Some("0.32.0"));
    assert_eq!(dist["ci"].as_array().unwrap()[0].as_str(), Some("github"));
    assert_eq!(dist["installers"].as_array().unwrap().len(), 3);
    for installer in ["shell", "powershell", "msi"] {
        assert!(
            dist["installers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str() == Some(installer)),
            "missing installer {installer}",
        );
    }
    assert_eq!(dist["targets"].as_array().unwrap().len(), 5);
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        assert!(
            dist["targets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str() == Some(target)),
            "missing target {target}",
        );
    }
    // No Homebrew tap: the formula publish job was dropped (unconfigured tap + token); shell,
    // PowerShell, and MSI installers are the supported paths.
    assert!(dist.get("tap").is_none());
    assert!(dist.get("publish-jobs").is_none());
    let binaries = manifest["bin"].as_array().unwrap();
    for binary in ["ccplan", "ccplan-fire"] {
        assert!(
            binaries
                .iter()
                .any(|value| value["name"].as_str() == Some(binary)),
            "missing package binary {binary}",
        );
    }

    let extra_artifacts = dist["extra-artifacts"].as_array().unwrap();
    let artifacts = extra_artifacts[0]["artifacts"].as_array().unwrap();
    for artifact in [
        "target/dist-assets/completions/ccplan.bash",
        "target/dist-assets/completions/_ccplan",
        "target/dist-assets/completions/ccplan.fish",
        "target/dist-assets/completions/_ccplan.ps1",
        "target/dist-assets/man/ccplan.1",
    ] {
        assert!(
            artifacts
                .iter()
                .any(|value| value.as_str() == Some(artifact)),
            "missing release artifact {artifact}",
        );
    }
    assert_eq!(
        extra_artifacts[0]["build"].as_array().unwrap()[0].as_str(),
        Some("bash"),
    );
}

#[test]
fn release_workflows_and_badges_are_real() {
    assert_file_contains(
        ".github/workflows/release-plz.yml",
        &[
            "release-plz/action@v0.5",
            "release-plz release-pr",
            "release-plz release",
            "branches: [main]",
        ],
    );
    assert_file_contains(
        ".github/workflows/release.yml",
        &["dist plan", "dist build", "dist host", "tags:"],
    );
    assert_file_contains(
        "wix/main.wxs",
        &["UpgradeCode", "ccplan.exe", "ccplan-fire.exe"],
    );

    let readme = read_repo_file("README.md");
    assert!(readme.contains("actions/workflows/ci.yml/badge.svg"));
    assert!(readme.contains("img.shields.io/github/v/release/ankitkpandey1/ccplan"));
    // crates.io badge is present now that the crate is published.
    assert!(readme.contains("img.shields.io/crates/v/"));
    assert!(!readme.contains("codecov.io"));
    assert!(!readme.contains("CI-pending"));
    assert!(!readme.contains("crates.io-unpublished"));
}
