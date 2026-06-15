use std::{
    fs,
    path::{Path, PathBuf},
};

use assert_cmd::Command;
use assert_fs::TempDir;
use serde_json::Value;

const RECIPE_START: &str = "<!-- ccplan-agent-recipe:start -->";
const RECIPE_END: &str = "<!-- ccplan-agent-recipe:end -->";
const PLAN_START: &str = "<!-- ccplan-test-plan:start -->";
const PLAN_END: &str = "<!-- ccplan-test-plan:end -->";
const COMMANDS_START: &str = "<!-- ccplan-test-commands:start -->";
const COMMANDS_END: &str = "<!-- ccplan-test-commands:end -->";

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_repo_file(relative: &str) -> String {
    fs::read_to_string(repo_path(relative)).unwrap_or_else(|error| {
        panic!("failed to read {relative}: {error}");
    })
}

fn extract_between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let start_index = text
        .find(start)
        .unwrap_or_else(|| panic!("missing start marker {start}"));
    let body_start = start_index + start.len();
    let end_index = text[body_start..]
        .find(end)
        .map(|index| body_start + index)
        .unwrap_or_else(|| panic!("missing end marker {end}"));
    text[body_start..end_index].trim()
}

fn skill_frontmatter(skill: &str) -> &str {
    let rest = skill
        .strip_prefix("---\n")
        .expect("SKILL.md must start with YAML frontmatter");
    let end = rest
        .find("\n---")
        .expect("SKILL.md frontmatter must be closed");
    &rest[..end]
}

fn frontmatter_field<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    frontmatter
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .map(str::trim)
        .map(|value| value.trim_matches('"'))
}

fn ccplan(temp: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ccplan"));
    command
        .env("CCPLAN_ROOT", temp.path())
        .env("CCPLAN_TEST_FAKE_BACKENDS", "1")
        .env("CCPLAN_TEST_NOW", "2099-01-01T08:00:00+00:00[UTC]");
    command
}

#[test]
fn skill_frontmatter_has_required_metadata() {
    let skill = read_repo_file("skills/ccplan/SKILL.md");
    let frontmatter = skill_frontmatter(&skill);

    assert_eq!(frontmatter_field(frontmatter, "name"), Some("ccplan"));
    let description = frontmatter_field(frontmatter, "description").unwrap_or_default();
    assert!(description.contains("Use when"));
    assert!(description.contains("ccplan"));
    assert!(frontmatter.contains("short-description:"));
    assert!(frontmatter.contains("when-to-use:"));
}

#[test]
fn agents_and_skill_share_the_canonical_recipe() {
    let agents = read_repo_file("AGENTS.md");
    let skill = read_repo_file("skills/ccplan/SKILL.md");
    let agents_recipe = extract_between(&agents, RECIPE_START, RECIPE_END);
    let skill_recipe = extract_between(&skill, RECIPE_START, RECIPE_END);

    assert_eq!(agents_recipe, skill_recipe);
    for required in [
        "cargo binstall -y ccplan",
        "ccplan --version",
        "ccplan doctor",
        "ccplan set --from -",
        "ccplan apply",
        "ccplan show --json",
        "ccplan agenda --json",
        "Exit codes",
        "JSON contract",
    ] {
        assert!(
            agents_recipe.contains(required),
            "canonical recipe should mention {required:?}",
        );
    }
}

#[test]
fn documented_agent_recipe_runs_against_a_headless_temp_store() {
    let skill = read_repo_file("skills/ccplan/SKILL.md");
    let plan = extract_between(&skill, PLAN_START, PLAN_END);
    let commands = extract_between(&skill, COMMANDS_START, COMMANDS_END)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    let temp = TempDir::new().unwrap();

    for command in commands {
        match command {
            "ccplan --version" => {
                ccplan(&temp).arg("--version").assert().success();
            }
            "ccplan doctor" => {
                ccplan(&temp)
                    .arg("doctor")
                    .assert()
                    .success()
                    .stdout(predicates::str::contains("scheduler"));
            }
            "ccplan set --from -" => {
                ccplan(&temp)
                    .args(["set", "--from", "-"])
                    .write_stdin(plan)
                    .assert()
                    .success();
            }
            "ccplan apply" => {
                ccplan(&temp).arg("apply").assert().success();
            }
            "ccplan show --json" => {
                let assert = ccplan(&temp).args(["show", "--json"]).assert().success();
                let json: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
                assert_eq!(json["date"], "2099-01-01");
                assert_eq!(json["block"][0]["id"], "focus-1");
            }
            "ccplan agenda --json" => {
                let assert = ccplan(&temp).args(["agenda", "--json"]).assert().success();
                let json: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
                assert!(json.as_array().is_some_and(|blocks| !blocks.is_empty()));
            }
            other => panic!("unhandled documented command {other:?}"),
        }
    }
}
