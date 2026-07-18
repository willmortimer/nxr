use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

mod common;

use common::{repo_root, require_nix};

#[test]
fn help_flag_succeeds() {
    cargo_bin_cmd!("nxr")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("List available flake apps"));
}

#[test]
fn version_flag_succeeds() {
    cargo_bin_cmd!("nxr")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("nxr"));
}

#[test]
fn reserved_command_reports_unimplemented() {
    cargo_bin_cmd!("nxr")
        .arg("run")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn unknown_subcommand_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .arg("not-a-command")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn list_fixture_apps_human() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available apps for"))
        .stdout(predicate::str::contains("hello"))
        .stdout(predicate::str::contains("fail"))
        .stdout(predicate::str::contains("echo-args"))
        .stdout(predicate::str::contains("succeed"))
        .stdout(predicate::str::contains("pwd"));
}

#[test]
fn list_fixture_apps_json() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--json", "list"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse json list output");

    assert_eq!(value["schema_version"], 1);
    assert!(value["flake"].is_string());
    assert!(value["system"].is_string());

    let apps = value["apps"].as_array().expect("apps array");
    assert!(!apps.is_empty());

    for app in apps {
        assert!(app["name"].is_string());
        assert!(app["attr_path"].is_string());
        assert!(app["default"].is_boolean());
    }

    let names: Vec<&str> = apps
        .iter()
        .map(|app| app["name"].as_str().expect("app name"))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(
        names, sorted,
        "apps must be sorted lexicographically by name"
    );

    assert!(names.contains(&"hello"));
    assert!(names.contains(&"fail"));
}

#[test]
fn list_fixture_apps_json_bare_list_defaults_to_list_command() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"));
}

#[test]
fn list_app_metadata_fixture_includes_descriptions() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/app-metadata", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available apps for"));

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(stdout.contains("Run static analysis"));
    assert!(stdout.contains("Run the test suite"));
    assert!(stdout.contains("Deploy the current revision"));
}

#[test]
fn list_from_nested_cwd_discovers_flake_without_flake_flag() {
    let Some(()) = require_nix() else {
        return;
    };

    let nested_cwd = repo_root().join("fixtures/nested-directory/deep/down/here");
    cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("Available apps for"))
        .stdout(predicate::str::contains("pwd"))
        .stdout(predicate::str::contains("root-marker"));
}

#[test]
fn list_broken_flake_exits_with_evaluation_error() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/broken-flake", "list"])
        .assert()
        .failure()
        .code(predicate::in_iter([3, 5]))
        .stderr(predicate::str::is_empty().not());
}

#[test]
fn list_basic_apps_lexicographic_sort_is_stable() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--json", "list"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse json list output");
    let apps = value["apps"].as_array().expect("apps array");

    let names: Vec<&str> = apps
        .iter()
        .map(|app| app["name"].as_str().expect("app name"))
        .collect();
    let expected = ["default", "echo-args", "fail", "hello", "pwd", "succeed"];
    assert_eq!(names, expected);
}
