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
fn select_without_tty_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .arg("select")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "interactive selection requires a terminal",
        ));
}

#[test]
fn global_select_without_tty_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .arg("--select")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "interactive selection requires a terminal",
        ));
}

#[test]
fn run_without_app_is_usage_error() {
    cargo_bin_cmd!("nxr").arg("run").assert().failure().code(2);
}

#[test]
fn unknown_app_exits_not_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "not-a-command"])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("app not found"));
}

#[test]
fn unknown_app_suggests_close_match() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "helo"])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("app not found: helo"))
        .stderr(predicate::str::contains("Did you mean:"))
        .stderr(predicate::str::contains("hello"));
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

#[test]
fn run_hello_prints_greeting() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from basic-apps"));
}

#[test]
fn run_inline_flake_app_ref_succeeds() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .arg("fixtures/basic-apps#hello")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from basic-apps"));
}

#[test]
fn inline_flake_app_ref_conflicts_with_flake_flag() {
    cargo_bin_cmd!("nxr")
        .args([
            "--flake",
            "fixtures/basic-apps",
            "fixtures/basic-apps#hello",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot use --flake with an inline flake#app reference",
        ));
}

#[test]
fn run_fail_propagates_exit_code() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "fail"])
        .assert()
        .failure()
        .code(42);
}

#[test]
fn plan_hello_json_matches_schema_shape() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "plan", "hello", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["kind"], "app");
    assert_eq!(value["target"], "hello");
    assert!(
        value["flake"]
            .as_str()
            .expect("flake")
            .contains("basic-apps")
    );
    assert!(value["command"]["program"].is_string());
    let arguments = value["command"]["arguments"]
        .as_array()
        .expect("command.arguments");
    assert!(arguments.len() >= 2);
    assert_eq!(arguments[0], "run");
    assert!(
        arguments[1]
            .as_str()
            .expect("installable")
            .ends_with("#hello")
    );
    assert!(
        value["forwarded_arguments"]
            .as_array()
            .expect("forwarded")
            .is_empty()
    );
}

#[test]
fn dry_run_prints_plan_without_executing() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--dry-run", "fail"])
        .assert()
        .success()
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("#fail"));
}

#[test]
fn run_echo_args_strips_one_separator() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "echo-args",
            "--",
            "alpha",
            "beta",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha\nbeta\n"));
}

#[test]
fn run_explicit_run_subcommand_executes_app() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "run", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from basic-apps"));
}

#[test]
fn run_pwd_from_nested_cwd_preserves_invocation_directory() {
    let Some(()) = require_nix() else {
        return;
    };

    let nested_cwd = repo_root().join("fixtures/nested-directory/deep/down/here");
    let expected = nested_cwd
        .canonicalize()
        .expect("canonicalize nested cwd")
        .to_string_lossy()
        .into_owned();

    cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .arg("pwd")
        .assert()
        .success()
        .stdout(predicate::str::contains(expected));
}

#[test]
fn run_root_flag_executes_from_flake_root() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let nested_cwd = repo_root.join("fixtures/nested-directory/deep/down/here");
    let flake_root = repo_root
        .join("fixtures/nested-directory")
        .canonicalize()
        .expect("canonicalize flake root")
        .to_string_lossy()
        .into_owned();

    cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args(["--root", "pwd"])
        .assert()
        .success()
        .stdout(predicate::str::contains(flake_root));
}

#[test]
fn run_cwd_flag_sets_child_working_directory() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let target = repo_root
        .canonicalize()
        .expect("canonicalize repo root")
        .to_string_lossy()
        .into_owned();

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "-C",
            target.as_str(),
            "pwd",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(target));
}

#[test]
fn plan_root_json_sets_execution_directory_to_flake_root() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let nested_cwd = repo_root.join("fixtures/nested-directory/deep/down/here");
    let flake_root = repo_root
        .join("fixtures/nested-directory")
        .canonicalize()
        .expect("canonicalize flake root")
        .to_string_lossy()
        .into_owned();

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args(["--root", "plan", "pwd", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");

    assert_eq!(value["target"], "pwd");
    assert_eq!(
        value["execution_directory"]
            .as_str()
            .expect("execution_directory"),
        flake_root
    );
}

#[test]
fn quiet_list_suppresses_runner_info_on_stderr() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--quiet", "--flake", "fixtures/basic-apps", "list"])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    assert!(
        !stderr.contains("discovering apps"),
        "quiet mode should suppress runner info on stderr, got:\n{stderr}"
    );
}

#[test]
fn verbose_run_emits_runner_diagnostics_on_stderr_not_stdout() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--verbose", "--flake", "fixtures/basic-apps", "hello"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");

    assert!(
        stdout.contains("hello from basic-apps"),
        "app output should remain on stdout, got:\n{stdout}"
    );
    assert!(
        stderr.contains("running app hello"),
        "verbose runner diagnostics should be on stderr, got:\n{stderr}"
    );
    assert!(
        !stdout.contains("running app hello"),
        "runner diagnostics must not appear on stdout, got:\n{stdout}"
    );
}

#[test]
fn completion_bash_emits_script() {
    cargo_bin_cmd!("nxr")
        .arg("completion")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("nxr"))
        .stdout(predicate::str::contains("_nxr_complete_apps"));
}

#[test]
fn completion_zsh_emits_script() {
    cargo_bin_cmd!("nxr")
        .arg("completion")
        .arg("zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef nxr"))
        .stdout(predicate::str::contains("_nxr_complete_apps"));
}

#[test]
fn completion_fish_emits_script() {
    cargo_bin_cmd!("nxr")
        .arg("completion")
        .arg("fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c nxr"))
        .stdout(predicate::str::contains("__nxr_complete_apps"));
}

#[test]
fn completion_unknown_shell_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .args(["completion", "powershell"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn manpage_writes_roff_to_stdout_only() {
    let assert = cargo_bin_cmd!("nxr")
        .arg("__manpage")
        .assert()
        .success()
        .stdout(predicate::str::contains(".TH nxr"))
        .stdout(predicate::str::contains("flake app runner"));

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    assert!(
        !stderr.contains("info:"),
        "__manpage must not emit runner diagnostics, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("discovering apps"),
        "__manpage must not emit runner diagnostics, got:\n{stderr}"
    );
}

#[test]
fn complete_apps_writes_only_to_stdout() {
    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["__complete", "apps"])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    assert!(
        !stderr.contains("discovering apps"),
        "__complete must not emit runner diagnostics, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("info:"),
        "__complete must not emit runner diagnostics, got:\n{stderr}"
    );
}

#[test]
fn global_output_flags_are_documented_in_help() {
    cargo_bin_cmd!("nxr")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--quiet"))
        .stdout(predicate::str::contains("--verbose"))
        .stdout(predicate::str::contains("--plain"))
        .stdout(predicate::str::contains("--no-color"))
        .stdout(predicate::str::contains("--color"))
        .stdout(predicate::str::contains("--log-format"))
        .stdout(predicate::str::contains("--clean-env"))
        .stdout(predicate::str::contains("--keep-env"))
        .stdout(predicate::str::contains("--set-env"))
        .stdout(predicate::str::contains("--unset-env"));
}

#[test]
fn doctor_help_documents_clean_env_flag() {
    cargo_bin_cmd!("nxr")
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--clean-env"))
        .stdout(predicate::str::contains("--all"));
}

#[test]
fn doctor_fixture_reports_nix_and_apps() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("info: nix.found:"))
        .stdout(predicate::str::contains("info: system.detected:"))
        .stdout(predicate::str::contains("info: flake.discovered:"))
        .stdout(predicate::str::contains("info: apps.listed:"));
}

#[test]
fn doctor_named_app_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "doctor", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "info: app.found: app found: hello",
        ));
}

#[test]
fn doctor_missing_app_exits_nonzero() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "doctor", "missing-app"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("error: app.missing:"));
}

#[test]
fn doctor_json_reports_findings_envelope() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--json", "doctor"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse doctor json");
    assert_eq!(value["schema_version"], 1);
    assert!(value["findings"].is_array());
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().expect("code"))
        .collect();
    assert!(codes.contains(&"nix.found"));
    assert!(codes.contains(&"apps.listed"));
}

#[test]
fn doctor_clean_env_reports_policy_without_executing_app() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "doctor",
            "--clean-env",
            "fail",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("info: clean_env.policy:"))
        .stdout(predicate::str::contains("info: plan.available:"))
        .stdout(predicate::str::contains("#fail"));
}

#[test]
fn graph_ci_text_lists_ordered_tasks() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "graph", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fmt\ntest\nci\n"));
}

#[test]
fn graph_ci_mermaid_contains_node_ids() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "graph",
            "ci",
            "--format",
            "mermaid",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("flowchart TD"))
        .stdout(predicate::str::contains("\"fmt\""))
        .stdout(predicate::str::contains("\"test\""))
        .stdout(predicate::str::contains("\"ci\""));
}

#[test]
fn graph_unknown_task_exits_not_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "graph", "missing-task"])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("unknown task root"));
}

#[test]
fn graph_json_envelope_includes_order_and_edges() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "--json", "graph", "ci"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse graph json");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["task"], "ci");
    assert_eq!(value["format"], "text");
    assert_eq!(
        value["order"].as_array().expect("order"),
        &[
            serde_json::json!("fmt"),
            serde_json::json!("test"),
            serde_json::json!("ci"),
        ]
    );
    let edges = value["edges"].as_array().expect("edges");
    assert!(
        edges
            .iter()
            .any(|edge| edge == &serde_json::json!(["fmt", "test"]))
    );
    assert!(
        edges
            .iter()
            .any(|edge| edge == &serde_json::json!(["test", "ci"]))
    );
}
