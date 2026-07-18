use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

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
#[ignore = "requires nix and fixture flakes"]
fn list_fixture_apps_human() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available apps for"));
}

#[test]
#[ignore = "requires nix and fixture flakes"]
fn list_fixture_apps_json() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    cargo_bin_cmd!("nxr")
        .current_dir(&repo_root)
        .args(["--flake", "fixtures/basic-apps", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"));
}
