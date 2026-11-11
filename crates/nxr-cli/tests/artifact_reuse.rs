//! A real `nxr task` run records producer receipts, and the next run skips them.

use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn second_task_run_skips_the_producer_chain() {
    if std::env::var_os("NXR_SKIP_NIX_INTEGRATION").is_some() || which::which("nix").is_err() {
        eprintln!("skipping integration test: nix is unavailable");
        return;
    }

    let root = tempfile::tempdir().expect("fixture");
    let home = tempfile::tempdir().expect("home");
    write_fixture(root.path());
    commit(root.path());

    let first = nxr(root.path(), home.path());
    assert!(
        first.status.success(),
        "cold run failed\nstdout: {}\nstderr: {}",
        first.stdout,
        first.stderr
    );
    assert!(
        !first.stderr.contains("\"type\":\"artifact_reused\""),
        "cold run reused a producer\n{}",
        first.stderr
    );
    assert_eq!(counter(root.path(), "a"), "x");
    assert_eq!(counter(root.path(), "b"), "x");
    assert_eq!(counter(root.path(), "c"), "x");
    assert_eq!(fs::read(root.path().join("out/c.bin")).unwrap(), b"c");

    let second = nxr(root.path(), home.path());
    assert!(
        second.status.success(),
        "warm run failed\nstdout: {}\nstderr: {}",
        second.stdout,
        second.stderr
    );
    assert_eq!(
        second
            .stderr
            .matches("\"type\":\"artifact_reused\"")
            .count(),
        3,
        "warm run did not reuse the chain\n{}",
        second.stderr
    );
    assert_eq!(counter(root.path(), "a"), "x");
    assert_eq!(counter(root.path(), "b"), "x");
    assert_eq!(counter(root.path(), "c"), "x");
    assert_eq!(fs::read(root.path().join("out/c.bin")).unwrap(), b"c");
    for (run_id, attempt) in reused_identities(&second.stderr) {
        assert!(run_id.starts_with("run-"), "{run_id}");
        assert!(attempt.starts_with("att-"), "{attempt}");
        assert_ne!(run_id, attempt);
    }

    let flake = fs::read_to_string(root.path().join("flake.nix")).unwrap();
    fs::write(
        root.path().join("flake.nix"),
        flake.replace("printf a > out/a.bin", "printf A > out/a.bin"),
    )
    .unwrap();
    commit(root.path());
    let third = nxr(root.path(), home.path());
    assert!(
        third.status.success(),
        "changed flake failed\nstdout: {}\nstderr: {}",
        third.stdout,
        third.stderr
    );
    assert!(
        !third.stderr.contains("\"type\":\"artifact_reused\""),
        "a flake.nix change reused a producer\n{}",
        third.stderr
    );
    assert_eq!(counter(root.path(), "a"), "xx");
    assert_eq!(counter(root.path(), "b"), "xx");
    assert_eq!(counter(root.path(), "c"), "xx");
}

fn reused_identities(stderr: &str) -> Vec<(String, String)> {
    stderr
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            if value.get("type")?.as_str()? != "artifact_reused" {
                return None;
            }
            Some((
                value.get("run_id")?.as_str()?.to_owned(),
                value.get("producer_attempt")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn nxr(root: &Path, home: &Path) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_nxr"))
        .current_dir(root)
        .env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("NXR_TRUST_PROJECT", "1")
        .args(["--flake", ".", "--events", "jsonl", "task", "c"])
        .output()
        .expect("spawn nxr");
    Run {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

struct Run {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn counter(root: &Path, node: &str) -> String {
    fs::read_to_string(root.join(".reuse").join(format!("{node}.count"))).unwrap_or_default()
}

fn commit(root: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init"]);
    git(&["add", "."]);
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.email=nxr@example.com", "-c", "user.name=nxr"])
        .args(["commit", "--no-verify", "-m", "fixture"])
        .status()
        .expect("commit");
    assert!(status.success(), "commit");
}

fn write_fixture(root: &Path) {
    fs::write(
        root.join(".gitignore"),
        "out/\n.reuse/\n.nxr/\nresult\nresult-*\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.txt"), "a").unwrap();
    fs::write(root.join("src/b.txt"), "b").unwrap();
    fs::write(root.join("src/c.txt"), "c").unwrap();
    fs::write(
        root.join("flake.lock"),
        include_str!("../../../fixtures/task-dag/flake.lock"),
    )
    .unwrap();
    fs::write(root.join("flake.nix"), FLAKE).unwrap();
}

const FLAKE: &str = r#"
{
  description = "artifact reuse chain";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "x86_64-darwin" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      mkApp = pkgs: name: text: {
        type = "app";
        program = "${pkgs.writeShellApplication { inherit name text; }}/bin/${name}";
      };
      doc = {
        schema_version = 2;
        tasks = {
          a = {
            app = "a";
            dependsOn = [ ];
            artifactReuse = true;
            paths = [ "src/a.txt" ];
            outputs = [ { path = "out/a.bin"; } ];
          };
          b = {
            app = "b";
            dependsOn = [ "a" ];
            artifactReuse = true;
            paths = [ "src/b.txt" ];
            outputs = [ { path = "out/b.bin"; } ];
          };
          c = {
            app = "c";
            dependsOn = [ "b" ];
            artifactReuse = true;
            paths = [ "src/c.txt" ];
            outputs = [ { path = "out/c.bin"; } ];
          };
        };
      };
    in
    {
      apps = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          a = mkApp pkgs "reuse-a" ''
            mkdir -p .reuse out
            printf x >> .reuse/a.count
            printf a > out/a.bin
          '';
          b = mkApp pkgs "reuse-b" ''
            mkdir -p .reuse out
            test -s out/a.bin
            printf x >> .reuse/b.count
            printf b > out/b.bin
          '';
          c = mkApp pkgs "reuse-c" ''
            mkdir -p .reuse out
            test -s out/b.bin
            printf x >> .reuse/c.count
            printf c > out/c.bin
          '';
        }
      );
      nxr = forAllSystems (_: doc);
    };
}
"#;
