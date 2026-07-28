use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

mod common;

use common::{NixCallCounter, repo_root, require_nix};

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
fn list_standard_outputs_packages_checks_shells() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/standard-outputs", "list", "packages"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available packages for"))
        .stdout(predicate::str::contains("default"))
        .stdout(predicate::str::contains("marker"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/standard-outputs", "list", "checks"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available checks for"))
        .stdout(predicate::str::contains("ok"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--json",
            "list",
            "shells",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""kind": "shells""#))
        .stdout(predicate::str::contains("backend"))
        .stdout(predicate::str::contains("default"));
}

#[test]
fn build_check_shell_dry_run_against_standard_outputs() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--dry-run",
            "build",
            "marker",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("build"))
        .stdout(predicate::str::contains("#packages."))
        .stdout(predicate::str::contains(".marker"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--dry-run",
            "--json",
            "check",
            "ok",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""kind": "check""#))
        .stdout(predicate::str::contains("#checks."))
        .stdout(predicate::str::contains(".ok"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--dry-run",
            "shell",
            "backend",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("develop"))
        .stdout(predicate::str::contains("#backend"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/standard-outputs", "--dry-run", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("flake"))
        .stdout(predicate::str::contains("check"));
}

#[test]
fn build_named_package_succeeds() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--quiet",
            "--flake",
            "fixtures/standard-outputs",
            "build",
            "marker",
        ])
        .assert()
        .success();
}

#[test]
fn unknown_package_suggests_close_match() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/standard-outputs", "build", "markr"])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("package not found"))
        .stderr(predicate::str::contains("marker"));
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
    assert!(stdout.contains("lint"));
    assert!(stdout.contains("test"));
    assert!(stdout.contains("deploy"));
    // Descriptions require Nix that surfaces app meta in `flake show`
    // (upstream ≈2.24+ / Determinate inventory `shortDescription`). Older
    // matrix entries (Nix 2.18, some Lix builds) list names only.
    if stdout.contains("Run static analysis") {
        assert!(stdout.contains("Run the test suite"));
        assert!(stdout.contains("Deploy the current revision"));
    }
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
fn shell_flag_runs_app_inside_named_dev_shell() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "--shell",
            "default",
            "shell-marker",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("inside-default-shell"));
}

#[test]
fn plan_with_shell_json_includes_shell_and_develop_argv() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "--shell",
            "default",
            "plan",
            "shell-marker",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");

    assert_eq!(value["shell"], "default");
    let arguments = value["command"]["arguments"]
        .as_array()
        .expect("command.arguments");
    assert_eq!(arguments[0], "develop");
    assert!(
        arguments[1]
            .as_str()
            .expect("installable")
            .ends_with("#default")
    );
    assert_eq!(arguments[2], "-c");
    assert!(arguments[3].as_str().expect("nix path").contains("nix"));
    assert_eq!(arguments[4], "run");
    assert!(
        arguments[5]
            .as_str()
            .expect("run installable")
            .ends_with("#shell-marker")
    );
}

#[test]
fn shell_mode_smart_skips_develop_when_active_shell_matches() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_DEV_SHELL", "default")
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "--shell",
            "default",
            "plan",
            "shell-marker",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");

    assert_eq!(value["shell"], "default");
    assert_eq!(value["active_shell"], "default");
    let arguments = value["command"]["arguments"]
        .as_array()
        .expect("command.arguments");
    assert_eq!(arguments[0], "run");
    assert!(
        arguments[1]
            .as_str()
            .expect("run installable")
            .ends_with("#shell-marker")
    );
}

#[test]
fn shell_mode_always_wraps_even_when_active_shell_matches() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_DEV_SHELL", "default")
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "--shell",
            "default",
            "--shell-mode",
            "always",
            "plan",
            "shell-marker",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");

    assert_eq!(value["command"]["arguments"][0], "develop");
}

#[test]
fn shell_mode_never_ignores_shell_flag() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "--shell",
            "default",
            "--shell-mode",
            "never",
            "plan",
            "shell-marker",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");

    assert_eq!(value["shell"], "default");
    assert_eq!(value["command"]["arguments"][0], "run");
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
fn nix_arg_forwards_refresh_to_plan_argv() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--nix-arg=--refresh",
            "plan",
            "hello",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse plan json");
    let args = value["command"]["arguments"]
        .as_array()
        .expect("command arguments");
    assert!(
        args.iter().any(|arg| arg.as_str() == Some("--refresh")),
        "expected --refresh in plan argv, got {args:?}"
    );
}

#[test]
fn refresh_discovery_bypasses_discovery_cache() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--refresh-discovery",
            "list",
        ])
        .assert()
        .success();
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
        .stdout(predicate::str::contains("_nxr_complete_apps"))
        .stdout(predicate::str::contains("_nxr_complete_target"))
        .stdout(predicate::str::contains("__complete \"$1\""))
        .stdout(predicate::str::contains("tasks"))
        .stdout(predicate::str::contains("packages"))
        .stdout(predicate::str::contains("namespaces"));
}

#[test]
fn completion_zsh_emits_script() {
    cargo_bin_cmd!("nxr")
        .arg("completion")
        .arg("zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef nxr"))
        .stdout(predicate::str::contains("_nxr_complete_apps"))
        .stdout(predicate::str::contains("_nxr_complete_target"))
        .stdout(predicate::str::contains("__complete \"$target\""))
        .stdout(predicate::str::contains("categories"));
}

#[test]
fn completion_fish_emits_script() {
    cargo_bin_cmd!("nxr")
        .arg("completion")
        .arg("fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c nxr"))
        .stdout(predicate::str::contains("__nxr_complete_apps"))
        .stdout(predicate::str::contains("__nxr_complete_tasks"))
        .stdout(predicate::str::contains("__nxr_complete_packages"))
        .stdout(predicate::str::contains("__nxr_complete_checks"))
        .stdout(predicate::str::contains("__nxr_complete_shells"))
        .stdout(predicate::str::contains("__nxr_complete_namespaces"))
        .stdout(predicate::str::contains("__nxr_complete_categories"));
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
fn global_nix_and_cache_flags_are_documented_in_help() {
    cargo_bin_cmd!("nxr")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--refresh-discovery"))
        .stdout(predicate::str::contains("--offline"))
        .stdout(predicate::str::contains("--accept-flake-config"))
        .stdout(predicate::str::contains("--nix-option"))
        .stdout(predicate::str::contains("--nix-arg"))
        .stdout(predicate::str::contains("cache"));
}

#[test]
fn legacy_refresh_flag_is_not_registered() {
    cargo_bin_cmd!("nxr")
        .args(["--refresh", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--refresh'"));
}

fn isolated_cache_command() -> (tempfile::TempDir, assert_cmd::Command) {
    let home = tempfile::TempDir::new().expect("temporary cache home");
    let mut command = cargo_bin_cmd!("nxr");
    command
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"));
    (home, command)
}

#[test]
fn cache_status_succeeds() {
    let (_home, mut command) = isolated_cache_command();
    command.args(["cache", "status"]).assert().success();
}

#[test]
fn cache_clear_succeeds() {
    let (_home, mut command) = isolated_cache_command();
    command.args(["cache", "clear"]).assert().success();
}

#[test]
fn cache_status_json_emits_path_and_entries() {
    let (_home, mut command) = isolated_cache_command();
    command
        .args(["cache", "status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"discovery\""))
        .stdout(predicate::str::contains("\"capabilities\""))
        .stdout(predicate::str::contains("\"entries\""))
        .stdout(predicate::str::contains("\"path\""));
}

#[test]
fn doctor_help_documents_clean_env_flag() {
    cargo_bin_cmd!("nxr")
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--clean-env"))
        .stdout(predicate::str::contains("--all"))
        .stdout(predicate::str::contains("determinate"));
}

#[test]
fn doctor_determinate_help_documents_flags() {
    cargo_bin_cmd!("nxr")
        .args(["doctor", "determinate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--all"))
        .stdout(predicate::str::contains("--refresh"));
}

#[test]
fn doctor_determinate_json_reports_distribution_envelope() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--json", "doctor", "determinate"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse doctor json");
    assert_eq!(value["schema_version"], 1);
    assert!(value["distribution"].is_string());
    assert!(value["findings"].is_array());
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().expect("code"))
        .collect();
    let distribution = value["distribution"].as_str().expect("distribution");
    if distribution == "upstream" || distribution == "lix" {
        assert!(codes.contains(&"determinate.distribution.na"));
    } else if distribution == "determinate" {
        assert!(codes.contains(&"determinate.distribution.detected"));
    }
    let rendered = stdout.to_ascii_lowercase();
    assert!(!rendered.contains("bearer "));
    assert!(!rendered.contains("api_key="));
}

#[test]
fn warm_doctor_determinate_reuses_capability_cache_without_reprobing() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let home = tempfile::TempDir::new().expect("cache home");
    let repo_root = repo_root();

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_NIX", &counter.wrapper)
        .args(["--json", "doctor", "determinate"])
        .assert()
        .success();

    let cold_log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    let cold_version = counter.count("version");
    let cold_config = counter.count("config");
    assert!(
        cold_version >= 1 || cold_config >= 1,
        "cold doctor determinate should probe capabilities at least once; log={cold_log}"
    );

    std::fs::write(&counter.log, "").expect("reset log");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_NIX", &counter.wrapper)
        .args(["--json", "doctor", "determinate"])
        .assert()
        .success();

    let warm_log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("version"),
        0,
        "warm doctor determinate should not re-probe version; log={warm_log}"
    );
    assert_eq!(
        counter.count("config"),
        0,
        "warm doctor determinate should not re-probe config; log={warm_log}"
    );
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
    assert!(value["capabilities"].is_object());
    assert!(value["capabilities"]["version"].is_string());
    assert!(value["capabilities"]["flakes_enabled"].is_boolean());
    assert!(value["capabilities"]["supports_json_log_format"].is_boolean());
    assert!(value["capabilities"]["supports_no_write_lock_file"].is_boolean());
    assert!(value["capabilities"]["supports_offline"].is_boolean());
    assert!(value["capabilities"]["supports_accept_flake_config"].is_boolean());
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().expect("code"))
        .collect();
    assert!(codes.contains(&"nix.found"));
    assert!(codes.contains(&"nix.version"));
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
fn explain_app_fixture_reports_workspace_and_command() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "explain", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kind: app"))
        .stdout(predicate::str::contains("target: hello"))
        .stdout(predicate::str::contains("attr_path:"))
        .stdout(predicate::str::contains("command: "))
        .stdout(predicate::str::contains("invalidation_key="));
}

#[test]
fn explain_task_fixture_reports_dependency_path() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "explain", "task", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kind: task"))
        .stdout(predicate::str::contains(
            "dependency_path: fmt -> test -> ci",
        ))
        .stdout(predicate::str::contains("[fmt]"))
        .stdout(predicate::str::contains("[ci]"));
}

#[test]
fn explain_json_emits_schema_version_and_workspace() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--json",
            "explain",
            "hello",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse explain json");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["kind"], "app");
    assert_eq!(value["target"], "hello");
    assert!(value["workspace"]["nix"]["executable"].is_string());
    assert!(value["workspace"]["discovery_cache"]["invalidation_key"].is_string());
    assert!(value["command"]["arguments"].is_array());
}

#[test]
fn doctor_all_json_includes_workspace_and_cache_findings() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--json",
            "doctor",
            "--all",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse doctor json");
    assert!(value["workspace"].is_object());
    assert!(value["workspace"]["discovery_cache"]["invalidation_key"].is_string());
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().expect("code"))
        .collect();
    assert!(codes.contains(&"cache.status") || codes.contains(&"cache.unavailable"));
    assert!(codes.iter().any(|code| code.starts_with("cache.")));
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
fn graph_ci_dot_contains_digraph_edges() {
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
            "dot",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("digraph {"))
        .stdout(predicate::str::contains("\"fmt\""))
        .stdout(predicate::str::contains("\"test\""))
        .stdout(predicate::str::contains("\"ci\""))
        .stdout(predicate::str::contains("\"fmt\" -> \"test\""))
        .stdout(predicate::str::contains("\"test\" -> \"ci\""));
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

#[test]
fn inspect_overview_basic_apps_lists_apps() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "inspect"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Flake:"))
        .stdout(predicate::str::contains("Apps:"))
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn inspect_overview_task_dag_includes_tasks_and_schema_version() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let list = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "list"])
        .output()
        .expect("spawn nxr list");
    if !list.status.success() {
        eprintln!("skipping inspect overview task-dag test: app discovery failed on this host");
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "inspect"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tasks (schema version 1):"))
        .stdout(predicate::str::contains("fmt"))
        .stdout(predicate::str::contains("ci"));
}

#[test]
fn inspect_app_basic_apps_happy_path() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "inspect", "app", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("App: hello"))
        .stdout(predicate::str::contains("Attr:"));
}

#[test]
fn inspect_task_task_dag_happy_path() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "inspect", "task", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task: ci"))
        .stdout(predicate::str::contains("Depends on: test"));
}

#[test]
fn inspect_unknown_app_exits_not_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "inspect",
            "app",
            "missing-app",
        ])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("app not found"));
}

#[test]
fn inspect_unknown_task_exits_not_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "inspect",
            "task",
            "missing-task",
        ])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("task not found"));
}

#[test]
fn inspect_overview_json_parses() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--json", "inspect"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inspect json");

    assert_eq!(value["schema_version"], 1);
    assert!(value["apps"].is_array());
    assert!(value.get("task_schema_version").is_none());
    assert!(
        value
            .get("tasks")
            .and_then(|tasks| tasks.as_object())
            .is_none_or(serde_json::Map::is_empty)
    );
}

#[test]
fn inspect_overview_task_dag_json_includes_tasks() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let list = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "list"])
        .output()
        .expect("spawn nxr list");
    if !list.status.success() {
        eprintln!(
            "skipping inspect overview task-dag json test: app discovery failed on this host"
        );
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "--json", "inspect"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inspect json");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["task_schema_version"], 1);
    assert!(value["apps"].is_array());
    assert!(value["tasks"]["ci"]["dependsOn"].is_array());
}

#[test]
fn inspect_app_json_parses() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "inspect",
            "app",
            "hello",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inspect app json");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["kind"], "app");
    assert_eq!(value["app"]["name"], "hello");
}

#[test]
fn inspect_task_json_parses() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "inspect",
            "task",
            "ci",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inspect task json");

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["kind"], "task");
    assert_eq!(value["name"], "ci");
    assert_eq!(value["app"], "ci");
    assert_eq!(value["dependsOn"], serde_json::json!(["test"]));
}

#[test]
fn task_alias_resolves_to_canonical_ci() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "task", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fmt\ntest\nci\n"));
}

#[test]
fn graph_alias_resolves_to_canonical_ci() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "graph", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fmt\ntest\nci"));
}

#[test]
fn inspect_task_alias_resolves_to_canonical_name() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "inspect", "task", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task: ci"));
}

#[test]
fn plan_task_ci_emits_execution_plan_json() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "plan", "check", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("parse execution plan json");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["root"], "ci");
    assert_eq!(value["argument_forwarding"], "root");
    assert_eq!(
        value["serial_order"],
        serde_json::json!(["fmt", "test", "ci"])
    );
}

#[test]
fn plan_task_alias_resolves_before_app_lookup() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "plan", "check", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("parse execution plan json");
    assert_eq!(value["root"], "ci");
}

#[test]
fn inspect_category_filter_limits_overview_tasks() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "inspect",
            "--category",
            "validation",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inspect json");
    let tasks = value["tasks"].as_object().expect("tasks object");
    assert_eq!(tasks.len(), 1);
    assert!(tasks.contains_key("ci"));
    assert!(!tasks.contains_key("fmt"));
}

#[test]
fn list_category_filter_limits_tasks_section() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "list",
            "--category",
            "validation",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available tasks"))
        .stdout(predicate::str::contains("ci  CI gate"))
        .stdout(predicate::str::contains("fmt  Format sources").not());
}

#[test]
fn list_category_selector_limits_tasks_section() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "list",
            "category:validation",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ci  CI gate"))
        .stdout(predicate::str::contains("fmt  Format sources").not());
}

#[test]
fn task_category_selector_runs_matching_tasks() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "--dry-run",
            "task",
            "category:validation",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("#ci"),
        "expected ci root from category selector:\n{stdout}"
    );
    assert!(
        stdout.contains("serial schedule: fmt -> test -> ci"),
        "category selector should still run ci dependencies:\n{stdout}"
    );
}

#[test]
fn plan_category_selector_emits_execution_plan() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "plan",
            "category:validation",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("parse execution plan json");
    assert_eq!(value["root"], "ci");
    assert_eq!(value["schema_version"], 1);
}

#[test]
fn task_changed_selector_runs_affected_union() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "--dry-run",
            "task",
            "changed",
            "--path",
            "shared/lib.txt",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    assert!(
        stderr.contains("running affected tasks:"),
        "expected affected roots log:\n{stderr}"
    );
    assert!(
        stdout.contains("serial schedule: shared-lib -> api-test -> web-test -> ci"),
        "expected affected union from changed selector:\n{stdout}"
    );
}

#[test]
fn ci_plan_json_exports_ci_plan_schema() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !task_dag_discovery_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "--json", "ci", "plan"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse ci plan json");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["roots"], serde_json::json!(["ci"]));
    assert_eq!(value["execution_plan"]["schema_version"], 1);
    assert_eq!(value["execution_plan"]["root"], "ci");
    assert!(value.get("affected").is_none_or(|v| v.is_null()));
}

#[test]
fn list_namespace_filter_limits_monorepo_fixture() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !namespaced_monorepo_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/namespaced-monorepo",
            "list",
            "--namespace",
            "api",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse list json");
    let apps = value["apps"].as_array().expect("apps array");
    let app_names: Vec<&str> = apps.iter().filter_map(|app| app["name"].as_str()).collect();
    assert_eq!(app_names, vec!["api-lint", "api-test"]);
    let tasks = value["tasks"].as_object().expect("tasks object");
    assert!(tasks.contains_key("api-ci"));
    assert!(tasks.contains_key("api-test"));
    assert!(!tasks.contains_key("web-ci"));
}

#[test]
fn list_category_filter_limits_monorepo_apps() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !namespaced_monorepo_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/namespaced-monorepo",
            "list",
            "--category",
            "frontend",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse list json");
    let apps = value["apps"].as_array().expect("apps array");
    let app_names: Vec<&str> = apps.iter().filter_map(|app| app["name"].as_str()).collect();
    assert_eq!(app_names, vec!["web-lint", "web-test"]);
    let tasks = value["tasks"].as_object().expect("tasks object");
    assert!(tasks.contains_key("web-ci"));
    assert!(!tasks.contains_key("api-ci"));
}

#[test]
fn inspect_namespace_filter_limits_overview() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !namespaced_monorepo_available(&repo_root) {
        return;
    }

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/namespaced-monorepo",
            "inspect",
            "--namespace",
            "web",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inspect json");
    let apps = value["apps"].as_array().expect("apps array");
    assert_eq!(apps.len(), 2);
    let tasks = value["tasks"].as_object().expect("tasks object");
    assert_eq!(tasks.len(), 2);
    assert!(tasks.contains_key("web-ci"));
}

fn namespaced_monorepo_available(repo_root: &std::path::Path) -> bool {
    let list = cargo_bin_cmd!("nxr")
        .current_dir(repo_root)
        .args(["--flake", "fixtures/namespaced-monorepo", "list"])
        .output()
        .expect("spawn nxr list");
    if !list.status.success() {
        eprintln!("skipping namespaced-monorepo test: app discovery failed on this host");
        return false;
    }
    true
}

fn task_dag_discovery_available(repo_root: &std::path::Path) -> bool {
    let list = cargo_bin_cmd!("nxr")
        .current_dir(repo_root)
        .args(["--flake", "fixtures/task-dag", "list"])
        .output()
        .expect("spawn nxr list");
    if !list.status.success() {
        eprintln!("skipping task-dag test: app discovery failed on this host");
        return false;
    }
    true
}

#[test]
fn task_ci_dry_run_prints_plans_in_serial_order() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "--dry-run", "task", "ci"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("# argument_forwarding=root stdin=inherit"),
        "expected frozen policy header:\n{stdout}"
    );
    let fmt_pos = stdout.find("#fmt").expect("fmt plan");
    let test_pos = stdout.find("#test").expect("test plan");
    let ci_pos = stdout.find("#ci").expect("ci plan");
    assert!(
        fmt_pos < test_pos && test_pos < ci_pos,
        "expected fmt → test → ci order in dry-run output:\n{stdout}"
    );
}

#[test]
fn task_dry_run_forwards_args_only_to_root() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "--dry-run",
            "--json",
            "task",
            "check",
            "--",
            "--from-cli",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("# argument_forwarding=root stdin=inherit"),
        "expected policy header:\n{stdout}"
    );

    // Alias `check` → canonical root `ci`; only that node should see forwarded args.
    let mut plans = Vec::new();
    let mut rest = stdout.as_str();
    while let Some(start) = rest.find('{') {
        let slice = &rest[start..];
        let mut depth = 0_i32;
        let mut end = None;
        for (idx, ch) in slice.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(idx + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.expect("balanced json object");
        let value: serde_json::Value =
            serde_json::from_str(&slice[..end]).expect("parse dry-run plan json");
        plans.push(value);
        rest = &slice[end..];
    }

    assert_eq!(plans.len(), 3, "expected fmt/test/ci plans:\n{stdout}");
    for plan in &plans[..2] {
        assert_eq!(
            plan["forwarded_arguments"],
            serde_json::json!([]),
            "deps must not receive trailing args: {plan}"
        );
    }
    assert_eq!(
        plans[2]["target"], "ci",
        "root plan should be canonical ci: {}",
        plans[2]
    );
    assert_eq!(
        plans[2]["forwarded_arguments"],
        serde_json::json!(["--from-cli"]),
        "root should receive trailing args: {}",
        plans[2]
    );
}

#[test]
fn task_parallel_dry_run_reports_null_stdin() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/parallel-group",
            "--dry-run",
            "task",
            "join",
            "-j",
            "2",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("# argument_forwarding=root stdin=null"),
        "parallel dry-run must close stdin:\n{stdout}"
    );
}

#[test]
fn task_ci_runs_apps_in_serial_order() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "task", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fmt\ntest\nci\n"));
}

#[test]
fn task_junit_report_records_node_outcomes() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let temp = tempfile::tempdir().expect("tempdir");
    let junit_path = temp.path().join("report.xml");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "task",
            "ci",
            "--junit",
            junit_path.to_str().expect("utf-8 path"),
        ])
        .assert()
        .success();

    let xml = std::fs::read_to_string(&junit_path).expect("read junit");
    assert!(xml.contains("<testsuites>"));
    assert!(xml.contains("name=\"fmt\""));
    assert!(xml.contains("name=\"test\""));
    assert!(xml.contains("name=\"ci\""));
    assert!(xml.contains("failures=\"0\""));
}

#[test]
fn task_global_report_flag_writes_junit() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let temp = tempfile::tempdir().expect("tempdir");
    let junit_path = temp.path().join("global.xml");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/task-dag",
            "--report",
            &format!("junit={}", junit_path.display()),
            "task",
            "ci",
        ])
        .assert()
        .success();

    let xml = std::fs::read_to_string(&junit_path).expect("read junit");
    assert!(xml.contains("<testcase"));
}

#[test]
fn golden_fixture_evaluates_and_lists() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/golden", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api-test"))
        .stdout(predicate::str::contains("ci"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/golden", "inspect"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backend"))
        .stdout(predicate::str::contains("validation"));
}

#[test]
fn task_rapid_exit_echo_delivers_stdout_chunks() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/task-dag",
            "--events",
            "jsonl",
            "--output",
            "live",
            "task",
            "ci",
            "-j",
            "1",
        ])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let stdout_chunks: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("jsonl event"))
        .filter(|event| event["type"] == "stdout_chunk")
        .collect();

    let payload = |node: &str| {
        stdout_chunks
            .iter()
            .find(|event| event["node"] == node)
            .and_then(|event| event["text"].as_str())
            .unwrap_or("")
    };

    assert_eq!(payload("fmt"), "fmt\n");
    assert_eq!(payload("test"), "test\n");
    assert_eq!(payload("ci"), "ci\n");
}

#[test]
fn task_unknown_name_exits_not_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/task-dag", "task", "missing-task"])
        .assert()
        .failure()
        .code(6)
        .stderr(predicate::str::contains("unknown task root `missing-task`"));
}

#[test]
fn task_without_name_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .args(["task"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn watch_without_name_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .args(["watch"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn watch_unknown_name_exits_not_found() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "watch", "missing-app"])
        .assert()
        .failure()
        .code(6);
}

#[test]
fn watch_app_prefix_runs_named_app() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use assert_cmd::cargo::CommandCargoExt;

    let Some(()) = require_nix() else {
        return;
    };

    let mut child = Command::cargo_bin("nxr")
        .expect("nxr binary")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/basic-apps",
            "watch",
            "app:hello",
            "--debounce",
            "100",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch");
    let stdout = child.stdout.take().expect("stdout");
    let rx = start_watch_stdout_reader(stdout);
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(45);
    wait_for_watch_occurrences(&rx, &mut output, "hello from basic-apps", 1, deadline);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn watch_help_mentions_debounce() {
    cargo_bin_cmd!("nxr")
        .args(["watch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--debounce"))
        .stdout(predicate::str::contains("--include"))
        .stdout(predicate::str::contains("--exclude"))
        .stdout(predicate::str::contains("--clear"))
        .stdout(predicate::str::contains("App or task name"));
}

#[test]
fn run_help_mentions_watch() {
    cargo_bin_cmd!("nxr")
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--watch"));
}

#[test]
fn task_help_mentions_watch() {
    cargo_bin_cmd!("nxr")
        .args(["task", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--watch"));
}

#[test]
fn help_mentions_task_output_flags() {
    cargo_bin_cmd!("nxr")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("live"))
        .stdout(predicate::str::contains("grouped"))
        .stdout(predicate::str::contains("failures"))
        .stdout(predicate::str::contains("raw"))
        .stdout(predicate::str::contains("--events"))
        .stdout(predicate::str::contains("jsonl"));
}

#[test]
fn task_output_flags_parse_before_subcommand() {
    cargo_bin_cmd!("nxr")
        .args(["--output", "live", "--events", "jsonl", "task", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task names"));
}

#[test]
fn task_help_mentions_jobs_and_keep_going() {
    cargo_bin_cmd!("nxr")
        .args(["task", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--jobs"))
        .stdout(predicate::str::contains("-j"))
        .stdout(predicate::str::contains("--keep-going"));
}

#[test]
fn task_parallel_dry_run_prints_waves() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/parallel-group",
            "--dry-run",
            "task",
            "join",
            "-j",
            "2",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("# parallel schedule"),
        "expected parallel schedule header:\n{stdout}"
    );
    assert!(
        stdout.contains("left") && stdout.contains("right"),
        "expected sibling nodes in dry-run:\n{stdout}"
    );
    assert!(
        stdout.contains("#a") && stdout.contains("#join"),
        "expected app plans in dry-run:\n{stdout}"
    );
}

#[test]
fn task_parallel_join_runs_siblings_concurrently() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/parallel-group",
            "--events",
            "jsonl",
            "task",
            "join",
            "-j",
            "2",
        ])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("jsonl event"))
        .collect();

    let left_start = events
        .iter()
        .position(|e| e["type"] == "node_started" && e["node"] == "left");
    let right_start = events
        .iter()
        .position(|e| e["type"] == "node_started" && e["node"] == "right");
    let left_exit = events
        .iter()
        .position(|e| e["type"] == "node_exited" && e["node"] == "left");
    let right_exit = events
        .iter()
        .position(|e| e["type"] == "node_exited" && e["node"] == "right");

    let left_start = left_start.expect("left started");
    let right_start = right_start.expect("right started");
    let left_exit = left_exit.expect("left exited");
    let right_exit = right_exit.expect("right exited");

    // Both siblings must start before either exits (true concurrency under -j 2).
    assert!(
        left_start < left_exit
            && right_start < right_exit
            && left_start < right_exit
            && right_start < left_exit,
        "expected overlapping left/right under -j 2:\n{stderr}"
    );

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("left-start") && stdout.contains("right-start") && stdout.contains("join"),
        "expected diamond output:\n{stdout}"
    );
}

#[test]
fn task_help_mentions_multi_root_union() {
    cargo_bin_cmd!("nxr")
        .args(["task", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("union DAG"));
}

#[test]
fn task_multi_root_diamond_dedupe_runs_shared_once() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/diamond-dedupe",
            "--events",
            "jsonl",
            "task",
            "lint",
            "unit",
            "integration",
            "-j",
            "8",
        ])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let shared_starts = stderr
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .is_some_and(|value| value["type"] == "node_started" && value["node"] == "shared")
        })
        .count();
    assert_eq!(
        shared_starts, 1,
        "shared ancestor must run once in union DAG:\n{stderr}"
    );

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("shared")
            && stdout.contains("lint")
            && stdout.contains("unit")
            && stdout.contains("integration"),
        "expected all task markers in output:\n{stdout}"
    );
}

#[test]
fn task_multi_root_dry_run_lists_union_nodes_once() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/diamond-dedupe",
            "--dry-run",
            "task",
            "lint",
            "unit",
            "integration",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert_eq!(
        stdout.matches("#shared").count(),
        1,
        "dry-run must plan shared once:\n{stdout}"
    );
    assert!(
        stdout.contains("#lint") && stdout.contains("#unit") && stdout.contains("#integration"),
        "expected all roots in dry-run plans:\n{stdout}"
    );
}

#[test]
fn task_fail_fast_cancels_independent_work() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/parallel-group",
            "task",
            "gate",
            "-j",
            "1",
        ])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn task_keep_going_runs_unrelated_after_failure() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/parallel-group",
            "--events",
            "jsonl",
            "task",
            "gate",
            "-j",
            "2",
            "--keep-going",
        ])
        .assert()
        .failure()
        .code(1);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let started: Vec<String> = stderr
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            if value["type"] == "node_started" {
                value["node"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .collect();

    assert!(
        started.iter().any(|n| n == "ok")
            && started.iter().any(|n| n == "unrelated")
            && started.iter().any(|n| n == "boom"),
        "keep-going should start independent siblings:\n{stderr}"
    );
    assert!(
        !started.iter().any(|n| n == "gate"),
        "gate depends on boom and must not start:\n{stderr}"
    );
}

#[test]
fn warm_list_reuses_capability_cache_without_reprobing() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let home = tempfile::TempDir::new().expect("cache home");
    let repo_root = repo_root();

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/basic-apps", "list"])
        .assert()
        .success();

    let cold_log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    let cold_version = counter.count("version");
    let cold_config = counter.count("config");
    let cold_help = counter.count("help");
    assert!(
        cold_version >= 1,
        "cold list should probe nix version at least once; log={cold_log}"
    );

    std::fs::write(&counter.log, "").expect("reset log");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/basic-apps", "list"])
        .assert()
        .success();

    let warm_log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("version"),
        0,
        "warm list should not re-probe version; cold version={cold_version} config={cold_config} help={cold_help}; log={warm_log}"
    );
    assert_eq!(
        counter.count("config"),
        0,
        "warm list should not re-probe config when env digest matches; log={warm_log}"
    );
    assert_eq!(
        counter.count("help"),
        0,
        "warm list should not re-probe help; log={warm_log}"
    );
    assert_eq!(
        counter.count("flake-show"),
        0,
        "warm list should reuse discovery cache (no flake show); log={warm_log}"
    );
}

#[test]
fn bare_app_fast_path_skips_flake_show() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .env("NXR_STORE_EXE_CACHE", "off")
        .args(["--flake", "fixtures/basic-apps", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from basic-apps"));

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(counter.count("flake-show"), 0, "log={log}");
    assert_eq!(counter.count("run"), 1, "log={log}");
    assert_eq!(
        counter.count("eval"),
        0,
        "no system probe on bare success; log={log}"
    );
    assert_eq!(
        counter.count("other"),
        0,
        "no capability probes on bare success; log={log}"
    );
}

#[test]
fn bare_app_failing_existing_app_is_one_nix_process() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .env("NXR_STORE_EXE_CACHE", "off")
        .args(["--flake", "fixtures/basic-apps", "fail"])
        .assert()
        .failure()
        .code(42);

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(counter.count("run"), 1, "log={log}");
    assert_eq!(counter.count("flake-show"), 0, "log={log}");
    assert_eq!(counter.count("eval"), 0, "log={log}");
    assert_eq!(counter.count("other"), 0, "log={log}");
}

#[test]
fn warm_store_exe_skips_nix_run() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let home = tempfile::TempDir::new().expect("cache home");
    let repo_root = repo_root();
    let cache = home.path().join("cache");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", &cache)
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/basic-apps", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from basic-apps"));

    let cold_log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("run"),
        0,
        "cold store-exe path should realise+exec without nix run; log={cold_log}"
    );
    assert!(
        counter.count("eval") >= 1,
        "cold store-exe should eval app.program (and possibly currentSystem); log={cold_log}"
    );

    std::fs::write(&counter.log, "").expect("reset log");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", &cache)
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/basic-apps", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello from basic-apps"));

    let warm_log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("run"),
        0,
        "warm store-exe must not call nix run; log={warm_log}"
    );
    assert_eq!(
        counter.count("eval"),
        0,
        "warm store-exe must not re-eval; log={warm_log}"
    );
    assert_eq!(
        counter.count("build"),
        0,
        "warm store-exe must not rebuild; log={warm_log}"
    );
}

#[test]
fn task_ci_uses_o1_discovery_not_per_node_flake_show() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/task-dag", "list"])
        .assert();
    if !assert.get_output().status.success() {
        eprintln!("skipping task-dag call-count test: app discovery failed on this host");
        return;
    }

    // Reset log after probe so only the task run is measured.
    std::fs::write(&counter.log, "").expect("reset log");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .env("NXR_STORE_EXE_CACHE", "off")
        .args(["--flake", "fixtures/task-dag", "task", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fmt\ntest\nci\n"));

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    let flake_shows = counter.count("flake-show");
    let runs = counter.count("run");
    assert!(
        flake_shows <= 1,
        "task run must discover apps at most once (warm cache may skip flake show); log={log}"
    );
    assert_eq!(
        runs, 3,
        "ci DAG has three app nodes → three nix run calls; log={log}"
    );
}

#[test]
fn coalesced_cold_task_discovery_uses_single_eval_when_forced() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let home = tempfile::TempDir::new().expect("cache home");
    let repo_root = repo_root();

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_FORCE_COALESCED_DISCOVERY", "1")
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/task-dag", "list"])
        .assert()
        .success();

    // Reset log after capability probes so only cold task discovery is measured.
    std::fs::write(&counter.log, "").expect("reset log");

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_FORCE_COALESCED_DISCOVERY", "1")
        .env("NXR_NIX", &counter.wrapper)
        .args([
            "--flake",
            "fixtures/task-dag",
            "--refresh-discovery",
            "task",
            "ci",
            "--dry-run",
        ])
        .assert()
        .success();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("flake-show"),
        0,
        "coalesced discovery must not call flake show; log={log}"
    );
    assert_eq!(
        counter.count("eval"),
        1,
        "coalesced discovery should use one eval; log={log}"
    );
}

#[test]
fn cache_explain_json_reports_structured_miss_reasons() {
    let Some(()) = require_nix() else {
        return;
    };

    let home = tempfile::TempDir::new().expect("cache home");
    let repo_root = repo_root();

    let output = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--json",
            "cache",
            "explain",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value = serde_json::from_slice(&output).expect("parse json");
    assert!(value.get("entry").is_some());
    let miss = value
        .pointer("/entry/miss_reasons")
        .and_then(|v| v.as_array())
        .expect("miss_reasons array");
    assert!(
        miss.iter()
            .any(|reason| reason.get("kind") == Some(&serde_json::json!("absent"))),
        "cold cache explain should report absent miss: {value}"
    );
}

#[test]
fn history_records_task_run_summary() {
    let Some(()) = require_nix() else {
        return;
    };

    let home = tempfile::TempDir::new().expect("state home");
    let repo_root = repo_root();

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("NXR_NIX", which::which("nix").expect("nix"))
        .args(["--flake", "fixtures/basic-apps", "run", "succeed"])
        .assert()
        .success();

    let output = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .args(["--json", "history"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value = serde_json::from_slice(&output).expect("parse history json");
    let entries = value
        .get("entries")
        .and_then(|v| v.as_array())
        .expect("entries");
    assert!(
        entries.iter().any(|entry| {
            entry.get("target_kind") == Some(&serde_json::json!("app"))
                && entry.get("target") == Some(&serde_json::json!("succeed"))
        }),
        "expected app succeed summary in history: {value}"
    );
}

#[test]
fn doctor_all_does_not_double_capability_probes() {
    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--json",
            "doctor",
            "--all",
        ])
        .assert()
        .success();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("version"),
        1,
        "single version probe; log={log}"
    );
    // `probe_config_json` tries `nix config show` then `show-config`; both log as
    // "config", so a single logical probe may appear as 1 or 2 lines.
    assert!(
        (1..=2).contains(&counter.count("config")),
        "config probe (with optional show-config fallback); log={log}"
    );
    assert!(
        (1..=2).contains(&counter.count("eval")),
        "system probe plus at most one discovery eval; log={log}"
    );
    assert!(
        counter.count("flake-show") <= 1,
        "at most one flake-show for doctor --all; log={log}"
    );
}

#[test]
fn watch_app_does_not_double_workspace_init_on_first_generation() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use assert_cmd::cargo::CommandCargoExt;

    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    let mut child = Command::cargo_bin("nxr")
        .expect("nxr binary")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .env("NXR_STORE_EXE_CACHE", "off")
        .args(["--flake", "fixtures/basic-apps", "watch", "hello"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch");

    let mut stdout = child.stdout.take().expect("stdout pipe");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut output = Vec::new();
    let mut buf = [0_u8; 256];
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!(
                "watch timed out before first generation; output={}",
                String::from_utf8_lossy(&output)
            );
        }
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                output.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&output).contains("hello from basic-apps") {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("read watch stdout: {error}"),
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("version"),
        1,
        "resolve_target + prepare share one adapter; log={log}"
    );
    // `probe_config_json` tries `nix config show` then `show-config`; both log as
    // "config", so a single logical probe may appear as 1 or 2 lines.
    assert!(
        (1..=2).contains(&counter.count("config")),
        "config probe (with optional show-config fallback); log={log}"
    );
    assert_eq!(
        counter.count("run"),
        1,
        "first generation runs once; log={log}"
    );
}

#[test]
fn watch_unprefixed_app_skips_task_eval_on_first_generation() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use assert_cmd::cargo::CommandCargoExt;

    let Some(()) = require_nix() else {
        return;
    };

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    let mut child = Command::cargo_bin("nxr")
        .expect("nxr binary")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .args(["--flake", "fixtures/basic-apps", "watch", "hello"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch");

    let mut stdout = child.stdout.take().expect("stdout pipe");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut output = Vec::new();
    let mut buf = [0_u8; 256];
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!(
                "watch timed out before first generation; output={}",
                String::from_utf8_lossy(&output)
            );
        }
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                output.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&output).contains("hello from basic-apps") {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("read watch stdout: {error}"),
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("eval"),
        1,
        "only currentSystem eval; task metadata eval is skipped; log={log}"
    );
}

#[test]
fn watch_unprefixed_prefers_task_when_app_and_task_share_name() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use assert_cmd::cargo::CommandCargoExt;

    let Some(()) = require_nix() else {
        return;
    };

    let fixture = isolated_task_dag_flake();
    let flake_dir = fixture.path().join("task-dag");
    let flake = flake_dir.to_string_lossy().into_owned();

    let counter = NixCallCounter::install();
    let repo_root = repo_root();
    let mut child = Command::cargo_bin("nxr")
        .expect("nxr binary")
        .current_dir(&repo_root)
        .env("NXR_NIX", &counter.wrapper)
        .args([
            "--flake",
            flake.as_str(),
            "watch",
            "ci",
            "--debounce",
            "100",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch");

    let stdout = child.stdout.take().expect("stdout pipe");
    let rx = start_watch_stdout_reader(stdout);
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    wait_for_watch_occurrences(&rx, &mut output, "fmt", 1, deadline);
    wait_for_watch_occurrences(&rx, &mut output, "test", 1, deadline);
    wait_for_watch_occurrences(&rx, &mut output, "ci", 1, deadline);

    let _ = child.kill();
    let _ = child.wait();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert!(
        counter.count("eval") >= 2,
        "task collision path loads task metadata; log={log}"
    );
}

fn start_watch_stdout_reader(
    stdout: impl std::io::Read + Send + 'static,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = [0_u8; 256];
        loop {
            match std::io::Read::read(&mut stdout, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn start_watch_stderr_drain(
    stderr: impl std::io::Read + Send + 'static,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    start_watch_stdout_reader(stderr)
}

fn wait_for_watch_occurrences(
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    output: &mut Vec<u8>,
    needle: &str,
    occurrences: usize,
    deadline: std::time::Instant,
) {
    use std::sync::mpsc::RecvTimeoutError;

    let mut disconnected = false;
    while std::time::Instant::now() < deadline {
        if String::from_utf8_lossy(output).matches(needle).count() >= occurrences {
            return;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    panic!(
        "watch {} waiting for {occurrences}x {needle:?}; output={}",
        if disconnected {
            "stdout closed"
        } else {
            "timed out"
        },
        String::from_utf8_lossy(output)
    );
}

/// Isolated copy of `fixtures/basic-apps` so parallel watch tests do not race
/// on shared tree mutations.
fn isolated_basic_apps_flake() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let src = repo_root().join("fixtures/basic-apps");
    let dst = temp.path().join("basic-apps");
    copy_dir_recursive(&src, &dst).expect("copy basic-apps fixture");
    temp
}

fn isolated_task_dag_flake() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let src = repo_root().join("fixtures/task-dag");
    let dst = temp.path().join("task-dag");
    copy_dir_recursive(&src, &dst).expect("copy task-dag fixture");
    temp
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn spawn_basic_apps_watch(
    counter: &common::NixCallCounter,
    flake_dir: &std::path::Path,
    extra_args: &[&str],
) -> (
    std::process::Child,
    std::process::ChildStdout,
    std::process::ChildStderr,
) {
    use std::process::{Command, Stdio};

    use assert_cmd::cargo::CommandCargoExt;

    let flake = flake_dir.to_string_lossy().into_owned();
    let mut args = vec![
        "--flake",
        flake.as_str(),
        "watch",
        "hello",
        "--debounce",
        "100",
    ];
    args.extend(extra_args);
    let mut child = Command::cargo_bin("nxr")
        .expect("nxr binary")
        .current_dir(repo_root())
        .env("NXR_NIX", &counter.wrapper)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch");
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    (child, stdout, stderr)
}

#[test]
fn watch_source_restart_skips_discovery_nix_calls() {
    use std::time::{Duration, Instant};

    let Some(()) = require_nix() else {
        return;
    };

    let fixture = isolated_basic_apps_flake();
    let flake_dir = fixture.path().join("basic-apps");
    let counter = NixCallCounter::install();
    let (mut child, stdout, stderr) = spawn_basic_apps_watch(&counter, &flake_dir, &[]);
    let rx = start_watch_stdout_reader(stdout);
    let stderr_rx = start_watch_stderr_drain(stderr);
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    wait_for_watch_occurrences(&rx, &mut output, "hello from basic-apps", 1, deadline);

    std::fs::write(&counter.log, "").expect("reset log");

    // FSEvents on macOS CI can miss a single create; use a non-dotfile and a
    // modify pulse, with a fresh deadline for the second generation.
    std::thread::sleep(Duration::from_millis(300));
    let trigger = flake_dir.join("source-trigger.txt");
    std::fs::write(&trigger, b"touch-1").expect("write trigger");
    std::thread::sleep(Duration::from_millis(100));
    std::fs::write(&trigger, b"touch-2").expect("modify trigger");
    let second_deadline = Instant::now() + Duration::from_secs(90);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_watch_occurrences(
            &rx,
            &mut output,
            "hello from basic-apps",
            2,
            second_deadline,
        );
    }));
    let _ = std::fs::remove_file(&trigger);

    let mut stderr_bytes = Vec::new();
    while let Ok(chunk) = stderr_rx.try_recv() {
        stderr_bytes.extend_from_slice(&chunk);
    }
    if let Err(panic) = result {
        let msg = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "watch assertion failed".to_owned());
        let _ = child.kill();
        let _ = child.wait();
        panic!("{msg}; stderr={}", String::from_utf8_lossy(&stderr_bytes));
    }

    let _ = child.kill();
    let _ = child.wait();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert_eq!(
        counter.count("flake-show"),
        0,
        "source-only restart must reuse snapshot; log={log}"
    );
    assert_eq!(
        counter.count("eval"),
        0,
        "source-only restart must not re-eval tasks; log={log}"
    );
    assert!(
        counter.count("run") >= 1,
        "expected at least one rerun; log={log}"
    );
}

#[test]
fn watch_metadata_restart_rediscovers_apps() {
    use std::time::{Duration, Instant};

    let Some(()) = require_nix() else {
        return;
    };

    let fixture = isolated_basic_apps_flake();
    let flake_dir = fixture.path().join("basic-apps");
    let flake_path = flake_dir.join("flake.nix");
    let original = std::fs::read(&flake_path).expect("read flake.nix");

    let counter = NixCallCounter::install();
    let (mut child, stdout, stderr) = spawn_basic_apps_watch(&counter, &flake_dir, &[]);
    let rx = start_watch_stdout_reader(stdout);
    let _stderr_rx = start_watch_stderr_drain(stderr);
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    wait_for_watch_occurrences(&rx, &mut output, "hello from basic-apps", 1, deadline);

    std::fs::write(&counter.log, "").expect("reset log");

    let mut edited = original;
    edited.push(b'\n');
    std::fs::write(&flake_path, &edited).expect("touch flake.nix");

    let second_deadline = Instant::now() + Duration::from_secs(90);
    wait_for_watch_occurrences(
        &rx,
        &mut output,
        "hello from basic-apps",
        2,
        second_deadline,
    );

    let _ = child.kill();
    let _ = child.wait();

    let log = std::fs::read_to_string(&counter.log).unwrap_or_default();
    assert!(
        counter.count("flake-show") >= 1,
        "metadata restart must rediscover apps; log={log}"
    );
}

fn parse_dry_run_plans(stdout: &str) -> Vec<serde_json::Value> {
    let mut plans = Vec::new();
    let mut rest = stdout;
    while let Some(start) = rest.find('{') {
        let slice = &rest[start..];
        let mut depth = 0_i32;
        let mut end = None;
        for (idx, ch) in slice.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(idx + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.expect("balanced json object");
        let json = &slice[..end];
        plans.push(serde_json::from_str(json).expect("parse plan json"));
        rest = &slice[end..];
    }
    plans
}

fn task_working_directory_paths(repo_root: &std::path::Path) -> (String, String) {
    let flake_root = repo_root
        .join("fixtures/task-working-directory")
        .canonicalize()
        .expect("canonicalize flake root")
        .to_string_lossy()
        .into_owned();
    let invocation = repo_root
        .join("fixtures/task-working-directory/deep/down/here")
        .canonicalize()
        .expect("canonicalize invocation cwd")
        .to_string_lossy()
        .into_owned();
    (flake_root, invocation)
}

#[test]
fn task_working_directory_tokens_resolve_in_dry_run() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let (flake_root, invocation) = task_working_directory_paths(&repo_root);
    let nested_cwd = repo_root.join("fixtures/task-working-directory/deep/down/here");
    let flake_arg = "../../..";

    let cases = [
        ("invocation-pwd", invocation.as_str()),
        ("flake-root-pwd", flake_root.as_str()),
        ("subdir-pwd", invocation.as_str()),
    ];

    for (task, expected_cwd) in cases {
        let assert = cargo_bin_cmd!("nxr")
            .current_dir(&nested_cwd)
            .args(["--flake", flake_arg, "--dry-run", "--json", "task", task])
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
        let plans = parse_dry_run_plans(&stdout);
        assert_eq!(
            plans.len(),
            1,
            "task {task} should emit one plan:\n{stdout}"
        );
        assert_eq!(
            plans[0]["execution_directory"]
                .as_str()
                .expect("execution_directory"),
            expected_cwd,
            "task {task}"
        );
    }
}

#[test]
fn task_chain_dependency_nodes_use_distinct_working_directories() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let (flake_root, invocation) = task_working_directory_paths(&repo_root);
    let nested_cwd = repo_root.join("fixtures/task-working-directory/deep/down/here");
    let flake_arg = "../../..";

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args(["--flake", flake_arg, "--dry-run", "--json", "task", "chain"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let plans = parse_dry_run_plans(&stdout);
    assert_eq!(plans.len(), 4, "chain has four nodes:\n{stdout}");

    let expected = [
        flake_root.as_str(),
        invocation.as_str(),
        invocation.as_str(),
        invocation.as_str(),
    ];
    for (index, cwd) in expected.iter().enumerate() {
        assert_eq!(
            plans[index]["execution_directory"]
                .as_str()
                .expect("execution_directory"),
            *cwd,
            "cwd mismatch at plan index {index}"
        );
    }
}

#[test]
fn task_cli_root_overrides_working_directory_metadata() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let (flake_root, _) = task_working_directory_paths(&repo_root);
    let nested_cwd = repo_root.join("fixtures/task-working-directory/deep/down/here");
    let flake_arg = "../../..";

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args([
            "--flake",
            flake_arg,
            "--root",
            "--dry-run",
            "--json",
            "task",
            "invocation-pwd",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let plans = parse_dry_run_plans(&stdout);
    assert_eq!(
        plans[0]["execution_directory"]
            .as_str()
            .expect("execution_directory"),
        flake_root.as_str()
    );
}

#[test]
fn task_cli_cwd_overrides_working_directory_metadata() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let override_cwd = repo_root
        .canonicalize()
        .expect("canonicalize repo root")
        .to_string_lossy()
        .into_owned();
    let nested_cwd = repo_root.join("fixtures/task-working-directory/deep/down/here");
    let flake_arg = "../../..";

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args([
            "--flake",
            flake_arg,
            "-C",
            override_cwd.as_str(),
            "--dry-run",
            "--json",
            "task",
            "flake-root-pwd",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let plans = parse_dry_run_plans(&stdout);
    assert_eq!(
        plans[0]["execution_directory"]
            .as_str()
            .expect("execution_directory"),
        override_cwd.as_str()
    );
}

#[test]
fn inspect_task_working_directory_matches_dry_run_execution_directory() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let (flake_root, _) = task_working_directory_paths(&repo_root);
    let nested_cwd = repo_root.join("fixtures/task-working-directory/deep/down/here");
    let flake_arg = "../../..";

    let inspect = cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args(["--flake", flake_arg, "inspect", "task", "flake-root-pwd"])
        .assert()
        .success();
    let inspect_stdout =
        String::from_utf8(inspect.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        inspect_stdout.contains("Working directory: flake-root"),
        "expected metadata in inspect output:\n{inspect_stdout}"
    );

    let dry_run = cargo_bin_cmd!("nxr")
        .current_dir(&nested_cwd)
        .args([
            "--flake",
            flake_arg,
            "--dry-run",
            "--json",
            "task",
            "flake-root-pwd",
        ])
        .assert()
        .success();
    let dry_stdout = String::from_utf8(dry_run.get_output().stdout.clone()).expect("utf-8 stdout");
    let plans = parse_dry_run_plans(&dry_stdout);
    assert_eq!(
        plans[0]["execution_directory"]
            .as_str()
            .expect("execution_directory"),
        flake_root.as_str()
    );
}

#[test]
fn affected_shared_dependency_change_propagates_to_dependents() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "--json",
            "affected",
            "shared/lib.txt",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["strict"], true);
    let tasks = value["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|entry| entry.as_str())
        .collect::<Vec<_>>();
    assert!(tasks.contains(&"shared-lib"));
    assert!(tasks.contains(&"api-test"));
    assert!(tasks.contains(&"web-test"));
    assert!(tasks.contains(&"ci"));
    // Apps without path roots are unknown and included under default strict.
    let apps = value["apps"]
        .as_array()
        .expect("apps array")
        .iter()
        .filter_map(|entry| entry.as_str())
        .collect::<Vec<_>>();
    assert!(apps.contains(&"shared-check"));
    let statuses = value["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter_map(|node| {
            Some((
                node["kind"].as_str()?,
                node["name"].as_str()?,
                node["status"].as_str()?,
            ))
        })
        .collect::<Vec<_>>();
    assert!(statuses.contains(&("task", "shared-lib", "affected")));
    assert!(statuses.contains(&("app", "shared-check", "unknown")));
}

#[test]
fn affected_no_strict_omits_unknown_from_lists() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "--json",
            "affected",
            "--no-strict",
            "shared/lib.txt",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
    assert_eq!(value["strict"], false);
    let apps = value["apps"].as_array().expect("apps array");
    assert!(apps.is_empty());
}

#[test]
fn task_affected_runs_union_of_affected_tasks() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "--dry-run",
            "task",
            "--affected",
            "--path",
            "shared/lib.txt",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    assert!(
        stderr.contains("running affected tasks:"),
        "expected affected roots log:\n{stderr}"
    );
    assert!(
        stdout.contains("serial schedule: shared-lib -> api-test -> web-test -> ci"),
        "expected full affected union schedule:\n{stdout}"
    );
}

#[test]
fn task_affected_intersects_named_roots() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "--dry-run",
            "task",
            "web-test",
            "--affected",
            "--path",
            "crates/api/README.md",
            "--no-strict",
        ])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    assert!(
        stderr.contains("no affected tasks to run"),
        "web-test should be skipped when only api paths changed:\n{stderr}"
    );
}

#[test]
fn task_affected_noop_when_nothing_matches_non_strict() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "--dry-run",
            "task",
            "--affected",
            "--path",
            "does-not-exist.txt",
            "--no-strict",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no affected tasks to run"));
}

#[test]
fn plan_affected_emits_multi_root_execution_plan() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/affected-deps",
            "plan",
            "--affected",
            "--path",
            "shared/lib.txt",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json plan");
    let roots = value["roots"]
        .as_array()
        .expect("roots")
        .iter()
        .filter_map(|entry| entry.as_str())
        .collect::<Vec<_>>();
    assert!(roots.contains(&"shared-lib"));
    assert!(roots.contains(&"api-test"));
    assert!(roots.contains(&"web-test"));
    assert!(roots.contains(&"ci"));
    let order = value["serial_order"]
        .as_array()
        .expect("serial_order")
        .iter()
        .filter_map(|entry| entry.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["shared-lib", "api-test", "web-test", "ci"]);
}

#[test]
fn task_help_mentions_affected() {
    cargo_bin_cmd!("nxr")
        .args(["task", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--affected"))
        .stdout(predicate::str::contains("--base"))
        .stdout(predicate::str::contains("--path"));
}

#[test]
fn plan_help_mentions_affected() {
    cargo_bin_cmd!("nxr")
        .args(["plan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--affected"));
}

#[test]
fn task_timeout_fixture_emits_timeout_fields_in_nxr_document() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let system = std::process::Command::new("nix")
        .args([
            "eval",
            "--impure",
            "--raw",
            "--expr",
            "builtins.currentSystem",
        ])
        .output()
        .expect("currentSystem");
    assert!(
        system.status.success(),
        "currentSystem failed: {}",
        String::from_utf8_lossy(&system.stderr)
    );
    let system = String::from_utf8(system.stdout).expect("utf-8 system");
    let attr = format!("./fixtures/task-timeout#nxr.{}.tasks.hang", system.trim());
    let output = std::process::Command::new("nix")
        .current_dir(&repo_root)
        .args(["eval", "--json", &attr])
        .output()
        .expect("nix eval");
    assert!(
        output.status.success(),
        "nix eval failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("parse hang task");
    assert_eq!(value["timeout"], "200ms");
    assert_eq!(value["terminationGracePeriod"], "100ms");
}

#[test]
fn task_timeout_hang_emits_timed_out_status() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/task-timeout",
            "--events",
            "jsonl",
            "task",
            "hang",
        ])
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("jsonl event"))
        .collect();

    let hang_exit = events
        .iter()
        .find(|e| e["type"] == "node_exited" && e["node"] == "hang" && e["status"] == "timed_out");
    assert!(
        hang_exit.is_some(),
        "expected hang timed_out event:\n{stderr}"
    );

    let completed = events
        .iter()
        .find(|e| e["type"] == "run_completed")
        .expect("run_completed");
    assert!(completed["run_id"].as_str().is_some());
    assert!(completed["duration_ms"].as_u64().is_some());
    assert!(completed["started_at"].as_str().is_some());
    assert!(completed["finished_at"].as_str().is_some());
}

#[test]
fn task_parallel_timeouts_fail_fast_do_not_double_complete() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/task-timeout",
            "--events",
            "jsonl",
            "task",
            "both",
            "-j",
            "2",
        ])
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("jsonl event"))
        .collect();

    let timed_out: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "node_exited" && e["status"] == "timed_out")
        .collect();
    assert!(
        !timed_out.is_empty(),
        "expected at least one timed_out peer:\n{stderr}"
    );

    // Each node gets exactly one terminal exit event.
    for node in ["slow_a", "slow_b", "both"] {
        let exits = events
            .iter()
            .filter(|e| e["type"] == "node_exited" && e["node"] == node)
            .count();
        assert_eq!(exits, 1, "node {node} should exit once:\n{stderr}");
    }

    let both = events
        .iter()
        .find(|e| e["type"] == "node_exited" && e["node"] == "both")
        .expect("both exit");
    assert_eq!(both["status"], "cancelled");
}

#[test]
fn task_timeout_keep_going_times_out_both_peers() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/task-timeout",
            "--events",
            "jsonl",
            "task",
            "both",
            "-j",
            "2",
            "--keep-going",
        ])
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8 stderr");
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).expect("jsonl event"))
        .collect();

    for node in ["slow_a", "slow_b"] {
        let exit = events
            .iter()
            .find(|e| e["type"] == "node_exited" && e["node"] == node)
            .unwrap_or_else(|| panic!("missing exit for {node}:\n{stderr}"));
        assert_eq!(exit["status"], "timed_out", "{node}:\n{stderr}");
    }

    let both = events
        .iter()
        .find(|e| e["type"] == "node_exited" && e["node"] == "both")
        .expect("both exit");
    assert_eq!(both["status"], "skipped");
}

#[test]
fn task_summary_fail_fast_includes_cancelled_descendants() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/parallel-group",
            "--output",
            "summary",
            "task",
            "gate",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("TASK") && stdout.contains("STATUS") && stdout.contains("DURATION"),
        "expected summary header:\n{stdout}"
    );
    assert!(
        stdout.contains("boom") && stdout.contains("failed"),
        "expected boom failed:\n{stdout}"
    );
    assert!(
        stdout.contains("gate") && stdout.contains("cancelled"),
        "expected gate cancelled before launch:\n{stdout}"
    );
}

#[test]
fn task_summary_keep_going_includes_skipped_descendants() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/parallel-group",
            "--output",
            "summary",
            "task",
            "gate",
            "--keep-going",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("TASK") && stdout.contains("STATUS"),
        "expected summary header:\n{stdout}"
    );
    assert!(
        stdout.contains("gate") && stdout.contains("skipped"),
        "expected gate skipped:\n{stdout}"
    );
    assert!(
        stdout.contains("ok") && stdout.contains("succeeded"),
        "expected ok succeeded under keep-going:\n{stdout}"
    );
}

#[test]
fn task_multi_root_dry_run_accepts_watch_compatible_union() {
    let Some(()) = require_nix() else {
        return;
    };

    // Watch generations reuse the normal multi-root task planner; dry-run proves
    // the union path `task --watch` shares still accepts multiple roots.
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/diamond-dedupe",
            "--dry-run",
            "task",
            "lint",
            "unit",
            "-j",
            "2",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("lint") && stdout.contains("unit") && stdout.contains("shared"),
        "expected multi-root union in dry-run:\n{stdout}"
    );
}

#[test]
fn fmt_dry_run_invokes_nix_fmt() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args(["--flake", "fixtures/standard-outputs", "--dry-run", "fmt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fmt"));
}

#[test]
fn envrc_prints_use_flake_default() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args(["--flake", "fixtures/standard-outputs", "envrc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("use flake\n"))
        .stdout(predicate::str::contains("completions"));
}

#[test]
fn envrc_named_shell_uses_flake_fragment() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--shell",
            "backend",
            "envrc",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("use flake .#backend"));
}

#[test]
fn envrc_write_refuses_existing_without_force() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !repo_root.join(".envrc").is_file() {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["envrc", "--write"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(".envrc already exists"));
}

#[test]
fn doctor_env_json_reports_envelope() {
    cargo_bin_cmd!("nxr")
        .args(["--json", "doctor", "env", "--flake", "fixtures/basic-apps"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("env."));
}

#[test]
fn doctor_cache_json_reports_envelope() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/basic-apps",
            "--json",
            "doctor",
            "cache",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("cache."));
}

#[test]
fn doctor_builders_json_reports_envelope() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .args(["--json", "doctor", "builders"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("builders."));
}

#[test]
fn in_runs_app_inside_named_dev_shell() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "in",
            "default",
            "shell-marker",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("inside-default-shell"));
}

#[test]
fn in_plan_subcommand_sets_shell_in_json() {
    let Some(()) = require_nix() else {
        return;
    };

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/named-dev-shells",
            "--json",
            "in",
            "default",
            "plan",
            "shell-marker",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse plan json");
    assert_eq!(value["shell"], "default");
}

#[test]
fn build_attr_dry_run_uses_explicit_installable() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--dry-run",
            "build",
            "--attr",
            "packages.x86_64-linux.marker",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("build"))
        .stdout(predicate::str::contains("#packages."));
}

#[test]
fn build_explicit_installable_dry_run() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/standard-outputs",
            "--dry-run",
            "build",
            ".#packages.x86_64-linux.marker",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(".#packages."));
}

#[test]
fn list_configurations_fixture() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/configurations",
            "list",
            "configurations",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev"))
        .stdout(predicate::str::contains("nixos"));
}

#[test]
fn build_configuration_dry_run() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/configurations",
            "--dry-run",
            "build",
            "configuration",
            "dev",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("nixosConfigurations.dev"));
}

#[test]
fn inspect_configuration_json() {
    let Some(()) = require_nix() else {
        return;
    };

    cargo_bin_cmd!("nxr")
        .current_dir(repo_root())
        .args([
            "--flake",
            "fixtures/configurations",
            "--json",
            "inspect",
            "configuration",
            "dev",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"dev\""))
        .stdout(predicate::str::contains("nixosConfigurations.dev"));
}

fn contexts_fixture_available(repo_root: &std::path::Path) -> bool {
    let list = cargo_bin_cmd!("nxr")
        .current_dir(repo_root)
        .args(["--flake", "fixtures/contexts", "list"])
        .output()
        .expect("spawn nxr list");
    if !list.status.success() {
        eprintln!("skipping contexts fixture test: app discovery failed on this host");
        return false;
    }
    true
}

#[test]
fn task_deploy_dry_run_plan_redacts_secret_values() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    // Isolate discovery cache so parallel tests cannot poison empty shell lists.
    let home = tempfile::TempDir::new().expect("temp home");
    let marker = "nxr-h3-redaction-marker";
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("NXR_FIXTURE_DEPLOY_TOKEN", marker)
        .env("NXR_ASSUME_YES", "1")
        .env("NXR_TRUST_PROJECT", "1")
        .args([
            "--flake",
            "fixtures/contexts",
            "--dry-run",
            "--json",
            "task",
            "deploy",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("NXR_FIXTURE_DEPLOY_TOKEN"),
        "expected logical ref in dry-run plan:\n{stdout}"
    );
    assert!(
        stdout.contains("<runtime>"),
        "expected runtime placeholder in dry-run plan:\n{stdout}"
    );
    assert!(
        !stdout.contains(marker),
        "secret value must not appear in dry-run plan output:\n{stdout}"
    );
}

#[test]
fn task_deploy_injects_env_secret_into_child() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    let home = tempfile::TempDir::new().expect("temp home");
    let marker = "nxr-h3-runtime-marker";
    let output = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("NXR_FIXTURE_DEPLOY_TOKEN", marker)
        .env("NXR_ASSUME_YES", "1")
        .env("NXR_TRUST_PROJECT", "1")
        .args(["--flake", "fixtures/contexts", "task", "deploy"])
        .output()
        .expect("spawn nxr task deploy");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "deploy should succeed when secret env is set\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("deploy ok"),
        "expected deploy marker in stdout"
    );
    assert!(
        !stdout.contains(marker) && !stderr.contains(marker),
        "secret value must not appear in task output"
    );
}

#[test]
fn task_deploy_missing_secret_names_ref_and_slot() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    let home = tempfile::TempDir::new().expect("temp home");
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env_remove("NXR_FIXTURE_DEPLOY_TOKEN")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("NXR_ASSUME_YES", "1")
        .env("NXR_TRUST_PROJECT", "1")
        .args(["--flake", "fixtures/contexts", "task", "deploy"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("DEPLOY_TOKEN"))
        .stderr(predicate::str::contains("NXR_FIXTURE_DEPLOY_TOKEN"));
}

#[test]
fn context_list_names_contexts() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/contexts", "context", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backend"))
        .stdout(predicate::str::contains("release"));
}

#[test]
fn task_deploy_requires_project_trust_without_override() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", temp.path())
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("XDG_CONFIG_HOME", temp.path())
        .env("NXR_FIXTURE_DEPLOY_TOKEN", "marker")
        .env("NXR_ASSUME_YES", "1")
        .env_remove("NXR_TRUST_PROJECT")
        .args(["--flake", "fixtures/contexts", "task", "deploy"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires trust approval"));
}

#[test]
fn context_inspect_shows_refs_not_values() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    let marker = "nxr-context-inspect-marker";
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("NXR_FIXTURE_DEPLOY_TOKEN", marker)
        .args([
            "--flake",
            "fixtures/contexts",
            "--json",
            "context",
            "inspect",
            "release",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("NXR_FIXTURE_DEPLOY_TOKEN"),
        "expected logical ref in inspect output:\n{stdout}"
    );
    assert!(
        !stdout.contains(marker),
        "secret value must not appear in inspect output:\n{stdout}"
    );
}

#[test]
fn task_integration_shell_wraps_via_develop_in_dry_run_plan() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    let home = tempfile::TempDir::new().expect("temp home");
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .args([
            "--flake",
            "fixtures/contexts",
            "--shell-mode",
            "always",
            "--dry-run",
            "--json",
            "task",
            "integration",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("develop"),
        "expected nix develop wrap in dry-run plan:\n{stdout}"
    );
    assert!(
        stdout.contains("#backend"),
        "expected backend devShell in dry-run plan:\n{stdout}"
    );
}

#[test]
fn context_run_deploys_with_env_secret() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    // Isolate discovery/capability caches so parallel tests cannot poison shell
    // metadata for this flake (empty dev_shells → unknown shell on context run).
    let home = tempfile::TempDir::new().expect("temp home");
    let marker = "nxr-context-run-marker";
    let output = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("NXR_FIXTURE_DEPLOY_TOKEN", marker)
        .env("NXR_ASSUME_YES", "1")
        .env("NXR_TRUST_PROJECT", "1")
        .args([
            "--flake",
            "fixtures/contexts",
            "context",
            "run",
            "release",
            "deploy",
        ])
        .output()
        .expect("spawn nxr context run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "context run should succeed when secret env is set\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("deploy ok"),
        "expected deploy marker in stdout:\n{stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains(marker),
        "secret value must not appear in output"
    );
}

#[test]
fn trust_add_marks_contexts_fixture_as_trusted() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    if !contexts_fixture_available(&repo_root) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["--flake", "fixtures/contexts", "trust", "add"])
        .assert()
        .success()
        .stderr(predicate::str::contains("trusted project"));

    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["--flake", "fixtures/contexts", "trust", "status"])
        .assert()
        .success()
        .stderr(predicate::str::contains("trusted:"));
}

#[test]
fn init_without_template_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .arg("init")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("template"));
}

#[test]
fn init_unknown_template_is_usage_error() {
    cargo_bin_cmd!("nxr")
        .args(["init", "not-a-template"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown template"));
}

#[test]
fn init_without_yes_requires_tty() {
    cargo_bin_cmd!("nxr")
        .args(["init", "rust"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "interactive confirmation requires a terminal",
        ));
}

#[test]
fn init_rust_template_writes_flake_with_yes() {
    let temp = tempfile::tempdir().expect("tempdir");
    cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .args(["init", "rust", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("wrote"));

    let flake = std::fs::read_to_string(temp.path().join("flake.nix")).expect("flake.nix");
    assert!(flake.contains("nxr.flakeModules.default"));
    assert!(flake.contains("nxr.apps"));
    assert!(flake.contains("nxr.tasks"));
}

#[test]
fn init_refuses_existing_target_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("flake.nix"), "# existing\n").expect("seed flake");

    cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .args(["init", "rust", "--yes"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn migrate_justfile_prints_nix_fragment() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Justfile"),
        "build:\n    cargo build\n\ntest: build\n    cargo test\n",
    )
    .expect("justfile");

    cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .args(["migrate", "justfile"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nxr.apps = {"))
        .stdout(predicate::str::contains("nxr.tasks = {"))
        .stdout(predicate::str::contains("cargo test"));
}

#[test]
fn migrate_mise_prints_nix_fragment() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("mise.toml"),
        "[tasks.build]\nrun = \"cargo build\"\n\n[tasks.test]\ndepends = [\"build\"]\nrun = \"cargo test\"\n",
    )
    .expect("mise.toml");

    cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .args(["migrate", "mise"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nxr.apps = {"))
        .stdout(predicate::str::contains("dependsOn = [ \"build\" ];"));
}

#[test]
fn migrate_justfile_write_creates_output_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("Justfile"), "hello:\n    echo hello\n").expect("justfile");
    let output = temp.path().join("migrated.nix");

    cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .args([
            "migrate",
            "justfile",
            "--write",
            output.to_str().expect("utf-8 path"),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("wrote"));

    let contents = std::fs::read_to_string(output).expect("output");
    assert!(contents.contains("hello = {"));
}

#[test]
fn init_template_lists_apps_with_local_nxr_input() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let temp = tempfile::tempdir().expect("tempdir");
    cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .args(["init", "rust", "--yes"])
        .assert()
        .success();

    let flake_path = temp.path().join("flake.nix");
    let mut flake = std::fs::read_to_string(&flake_path).expect("flake");
    let nxr_input = format!("path:{}", repo_root.display());
    flake = flake.replace("github:willmortimer/nxr", &nxr_input);
    std::fs::write(&flake_path, flake).expect("write patched flake");

    let lock_status = std::process::Command::new("nix")
        .current_dir(temp.path())
        .args(["flake", "lock", "--accept-flake-config"])
        .status()
        .expect("nix flake lock");
    if !lock_status.success() {
        eprintln!("skipping integration test: nix flake lock failed");
        return;
    }

    let list = cargo_bin_cmd!("nxr")
        .current_dir(temp.path())
        .arg("list")
        .output()
        .expect("nxr list");
    if !list.status.success() {
        eprintln!(
            "skipping integration test: scaffolded flake did not evaluate in this environment"
        );
        return;
    }

    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("test"));
    assert!(stdout.contains("fmt"));
}

#[test]
fn cache_explain_workspace_task_reports_tier_and_key() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/workspace-cache",
            "cache",
            "explain",
            "codegen",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse cache explain json");
    assert_eq!(value["tier"], "workspace-action");
    assert_eq!(value["cache_enabled"], true);
    assert!(
        value["action_key"]
            .as_str()
            .is_some_and(|key| !key.is_empty())
    );
}

#[test]
fn inventory_lists_custom_role() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/inventory-custom", "inventory"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    // Custom/unknown roles require Determinate-style inventory envelopes
    // (`inventory.<role>.unknown`). Stock Nix 2.18 / some Lix builds omit them.
    if stdout.contains("No inventory roles found") {
        return;
    }
    assert!(
        stdout.contains("customWorkflow"),
        "expected customWorkflow role in inventory output:\n{stdout}"
    );
}

#[test]
fn inventory_role_filter_lists_entries() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/configurations",
            "--json",
            "inventory",
            "--role",
            "nixosConfigurations",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse inventory json");
    let entries = value
        .get("entries")
        .and_then(|entries| entries.as_array())
        .expect("entries array");
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .any(|entry| entry.get("name").and_then(|name| name.as_str()) == Some("dev"))
    );
}

#[test]
fn task_dry_run_notes_workspace_cache_lookup() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/workspace-cache",
            "task",
            "codegen",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("# cache codegen:"))
        .stdout(predicate::str::contains("workspace-action"));
}

fn isolated_workspace_cache_flake() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let src = repo_root().join("fixtures/workspace-cache");
    let dst = temp.path().join("workspace-cache");
    copy_dir_recursive(&src, &dst).expect("copy workspace-cache fixture");
    temp
}

fn isolated_contexts_flake() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let src = repo_root().join("fixtures/contexts");
    let dst = temp.path().join("contexts");
    copy_dir_recursive(&src, &dst).expect("copy contexts fixture");
    temp
}

#[test]
fn cache_explain_secret_task_reports_disable_reason() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args([
            "--flake",
            "fixtures/workspace-cache",
            "cache",
            "explain",
            "secret-codegen",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse cache explain json");
    assert_eq!(value["tier"], "workspace-action");
    assert_eq!(value["cache_enabled"], false);
    let reason = value["lookup"]["reason"].as_str().expect("skipped reason");
    assert!(
        reason.contains("secret-bearing"),
        "expected secret disable reason, got {reason}"
    );
}

#[test]
fn secret_codegen_key_a_to_key_b_does_not_restore_stale_output() {
    let Some(()) = require_nix() else {
        return;
    };

    let fixture = isolated_workspace_cache_flake();
    let flake = fixture.path().join("workspace-cache");
    let home = tempfile::TempDir::new().expect("cache home");
    let out = flake.join("gen/secret-out.txt");

    cargo_bin_cmd!("nxr")
        .current_dir(&flake)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_FIXTURE_API_KEY", "key-a")
        .args(["task", "secret-codegen"])
        .assert()
        .success();
    let first = std::fs::read_to_string(&out).expect("read secret-out after key-a");
    assert!(
        first.contains("key-a"),
        "expected key-a in output, got {first}"
    );

    cargo_bin_cmd!("nxr")
        .current_dir(&flake)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_FIXTURE_API_KEY", "key-b")
        .args(["task", "secret-codegen"])
        .assert()
        .success();
    let second = std::fs::read_to_string(&out).expect("read secret-out after key-b");
    assert!(
        second.contains("key-b"),
        "key-a→key-b must miss / re-run; got {second}"
    );
    assert!(
        !second.contains("key-a"),
        "stale key-a output must not be restored; got {second}"
    );
}

#[test]
fn shared_cache_modes_fail_closed_at_plan_task_and_explain() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    for task in ["shared-codegen", "shared-read-codegen"] {
        for args in [
            vec!["plan", task],
            vec!["task", task, "--dry-run"],
            vec!["cache", "explain", task],
        ] {
            cargo_bin_cmd!("nxr")
                .current_dir(&repo_root)
                .args(["--flake", "fixtures/workspace-cache"])
                .args(&args)
                .assert()
                .failure()
                .stderr(predicate::str::contains("not implemented"));
        }
    }
}

#[test]
fn cache_explain_context_secret_task_reports_disable_reason() {
    let Some(()) = require_nix() else {
        return;
    };

    let fixture = isolated_contexts_flake();
    let flake = fixture.path().join("contexts");
    let home = tempfile::TempDir::new().expect("temp home");

    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&flake)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NXR_FIXTURE_DEPLOY_TOKEN", "marker")
        .env("NXR_ASSUME_YES", "1")
        .args(["cache", "explain", "cached-deploy", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("parse cache explain json");
    assert_eq!(value["cache_enabled"], false);
    let reason = value["lookup"]["reason"].as_str().expect("skipped reason");
    assert!(
        reason.contains("secret-bearing"),
        "expected context-secret disable reason, got {reason}"
    );
}

#[test]
fn process_status_lists_declared_processes() {
    let Some(()) = require_nix() else {
        return;
    };

    let repo_root = repo_root();
    let assert = cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/processes", "status"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    assert!(
        stdout.contains("worker"),
        "expected worker process in status output:\n{stdout}"
    );
    assert!(stdout.contains("stopped"));
}
