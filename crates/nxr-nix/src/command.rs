//! Nix argument-vector construction (no shell concatenation).

/// Environment variable overriding the `nix` executable path.
pub const NIX_EXECUTABLE_ENV: &str = "NXR_NIX";

/// Arguments for `nix eval --raw --impure --expr builtins.currentSystem`.
#[must_use]
pub fn current_system_args() -> Vec<String> {
    vec![
        "eval".to_owned(),
        "--raw".to_owned(),
        "--impure".to_owned(),
        "--expr".to_owned(),
        "builtins.currentSystem".to_owned(),
    ]
}

/// Arguments for `nix flake show --json <flake_ref>`.
#[must_use]
pub fn flake_show_args(flake_ref: &str) -> Vec<String> {
    vec![
        "flake".to_owned(),
        "show".to_owned(),
        "--json".to_owned(),
        flake_ref.to_owned(),
    ]
}

/// Arguments for `nix eval --json <flake_ref>#<attr_path>`.
///
/// Used for versioned metadata such as `nxr.<system>` (task documents).
#[must_use]
pub fn flake_eval_json_args(flake_ref: &str, attr_path: &str) -> Vec<String> {
    vec![
        "eval".to_owned(),
        "--json".to_owned(),
        format!("{flake_ref}#{attr_path}"),
    ]
}

/// Arguments for `nix run <flake_ref>#<app_name> [-- <forwarded_args…>]`.
///
/// Inserts exactly one `--` before forwarded args when any are present.
#[must_use]
pub fn nix_run_args(
    flake_ref: &str,
    app_name: &str,
    forwarded_args: &[impl AsRef<str>],
) -> Vec<String> {
    let mut args = vec!["run".to_owned(), format!("{flake_ref}#{app_name}")];

    if !forwarded_args.is_empty() {
        args.push("--".to_owned());
        args.extend(forwarded_args.iter().map(|arg| arg.as_ref().to_owned()));
    }

    args
}

/// Wrap a `nix run …` argv vector for execution inside a dev shell.
///
/// Equivalent to:
/// `nix develop <flake_ref>#<shell_name> -c <nix_program> <nix_run_argv…>`
///
/// `<nix_run_argv…>` is the same vector passed to the outer `nix` invocation
/// (for example `run <flake_ref>#<app> [-- <forwarded…>]`).
#[must_use]
pub fn nix_develop_wrap_run_args(
    nix_program: &str,
    flake_ref: &str,
    shell_name: &str,
    nix_run_argv: &[String],
) -> Vec<String> {
    let mut args = vec![
        "develop".to_owned(),
        format!("{flake_ref}#{shell_name}"),
        "-c".to_owned(),
        nix_program.to_owned(),
    ];
    args.extend_from_slice(nix_run_argv);
    args
}

/// Arguments for `nix develop <flake_ref>#<shell_name> -c <program> [args…]`.
///
/// Used to wrap arbitrary workspace scripts (or other non-`nix run` programs)
/// inside a development shell.
#[must_use]
pub fn nix_develop_wrap_command_args(
    flake_ref: &str,
    shell_name: &str,
    program: &str,
    program_args: &[impl AsRef<str>],
) -> Vec<String> {
    let mut args = vec![
        "develop".to_owned(),
        format!("{flake_ref}#{shell_name}"),
        "-c".to_owned(),
        program.to_owned(),
    ];
    args.extend(program_args.iter().map(|arg| arg.as_ref().to_owned()));
    args
}

/// Arguments for `nix build --no-link --print-out-paths <installable>`.
///
/// Used to realise a store output without creating a `./result` link.
#[must_use]
pub fn nix_build_no_link_print_out_paths_args(installable: &str) -> Vec<String> {
    vec![
        "build".to_owned(),
        "--no-link".to_owned(),
        "--print-out-paths".to_owned(),
        installable.to_owned(),
    ]
}

/// Arguments for `nix eval --raw <flake_ref>#apps.<system>.<app>.program`.
#[must_use]
pub fn flake_app_program_eval_args(flake_ref: &str, system: &str, app_name: &str) -> Vec<String> {
    vec![
        "eval".to_owned(),
        "--raw".to_owned(),
        format!("{flake_ref}#apps.{system}.{app_name}.program"),
    ]
}

/// Arguments for `nix build <installable>`.
///
/// `installable` is a full flake installable such as `.#packages.x86_64-linux.nxr`
/// or just `.` / `.#default` for the flake's default package.
#[must_use]
pub fn nix_build_args(installable: &str) -> Vec<String> {
    vec!["build".to_owned(), installable.to_owned()]
}

/// Arguments for `nix flake check <flake_ref>`.
#[must_use]
pub fn nix_flake_check_args(flake_ref: &str) -> Vec<String> {
    vec!["flake".to_owned(), "check".to_owned(), flake_ref.to_owned()]
}

/// Arguments for `nix print-dev-env <flake_ref>#<shell_name> --json`.
///
/// Used to materialize a process-compatible development environment snapshot
/// (ADR-0171); callers must fall back to develop-wrap when unsupported shell
/// semantics are present in the JSON output.
#[must_use]
pub fn print_dev_env_args(flake_ref: &str, shell_name: &str) -> Vec<String> {
    vec![
        "print-dev-env".to_owned(),
        format!("{flake_ref}#{shell_name}"),
        "--json".to_owned(),
    ]
}

/// Arguments for interactive `nix develop`.
///
/// When `shell_name` is `None`, enters the flake's default development shell
/// (`nix develop <flake_ref>`). Otherwise uses `nix develop <flake_ref>#<shell_name>`.
#[must_use]
pub fn nix_develop_args(flake_ref: &str, shell_name: Option<&str>) -> Vec<String> {
    match shell_name {
        Some(name) => vec!["develop".to_owned(), format!("{flake_ref}#{name}")],
        None => vec!["develop".to_owned(), flake_ref.to_owned()],
    }
}

/// Build installable for `packages.<system>.<name>`.
#[must_use]
pub fn package_installable(flake_ref: &str, system: &str, name: &str) -> String {
    format!("{flake_ref}#packages.{system}.{name}")
}

/// Build installable for `checks.<system>.<name>`.
#[must_use]
pub fn check_installable(flake_ref: &str, system: &str, name: &str) -> String {
    format!("{flake_ref}#checks.{system}.{name}")
}

/// Arguments for `nix fmt` (flake formatter).
///
/// When `paths` is empty, `nix fmt` formats the flake in the current directory.
#[must_use]
pub fn nix_fmt_args(paths: &[impl AsRef<str>]) -> Vec<String> {
    let mut args = vec!["fmt".to_owned()];
    args.extend(paths.iter().map(|path| path.as_ref().to_owned()));
    args
}

/// Build installable from a flake reference and attribute path.
#[must_use]
pub fn attr_installable(flake_ref: &str, attr_path: &str) -> String {
    format!("{flake_ref}#{attr_path}")
}

/// Whether a build token is an explicit flake installable rather than a leaf name.
#[must_use]
pub fn token_is_explicit_installable(token: &str) -> bool {
    token.contains('#')
        || token.starts_with(".#")
        || token.contains(':') && (token.starts_with("github:") || token.starts_with("git+"))
}

#[cfg(test)]
mod tests {
    use super::{
        attr_installable, check_installable, current_system_args, flake_app_program_eval_args,
        flake_eval_json_args, flake_show_args, nix_build_args,
        nix_build_no_link_print_out_paths_args, nix_develop_args, nix_develop_wrap_run_args,
        nix_flake_check_args, nix_fmt_args, nix_run_args, package_installable, print_dev_env_args,
        token_is_explicit_installable,
    };

    #[test]
    fn current_system_argv_matches_nix_contract() {
        assert_eq!(
            current_system_args(),
            vec![
                "eval".to_owned(),
                "--raw".to_owned(),
                "--impure".to_owned(),
                "--expr".to_owned(),
                "builtins.currentSystem".to_owned(),
            ]
        );
    }

    #[test]
    fn flake_show_argv_includes_json_flag_and_flake_ref() {
        assert_eq!(
            flake_show_args("."),
            vec![
                "flake".to_owned(),
                "show".to_owned(),
                "--json".to_owned(),
                ".".to_owned(),
            ]
        );
        assert_eq!(
            flake_show_args("./fixtures/basic-apps"),
            vec![
                "flake".to_owned(),
                "show".to_owned(),
                "--json".to_owned(),
                "./fixtures/basic-apps".to_owned(),
            ]
        );
    }

    #[test]
    fn flake_eval_json_argv_includes_json_flag_and_installable() {
        assert_eq!(
            flake_eval_json_args(".", "nxr.aarch64-darwin"),
            vec![
                "eval".to_owned(),
                "--json".to_owned(),
                ".#nxr.aarch64-darwin".to_owned(),
            ]
        );
        assert_eq!(
            flake_eval_json_args("./fixtures/task-dag", "nxr.x86_64-linux"),
            vec![
                "eval".to_owned(),
                "--json".to_owned(),
                "./fixtures/task-dag#nxr.x86_64-linux".to_owned(),
            ]
        );
    }

    #[test]
    fn nix_run_argv_without_forwarded_args_omits_separator() {
        assert_eq!(
            nix_run_args(".", "hello", &[] as &[String]),
            vec!["run".to_owned(), ".#hello".to_owned()]
        );
    }

    #[test]
    fn nix_run_argv_with_forwarded_args_inserts_separator() {
        assert_eq!(
            nix_run_args(
                "./fixtures/basic-apps",
                "echo-args",
                &["one".to_owned(), "two".to_owned()]
            ),
            vec![
                "run".to_owned(),
                "./fixtures/basic-apps#echo-args".to_owned(),
                "--".to_owned(),
                "one".to_owned(),
                "two".to_owned(),
            ]
        );
    }

    #[test]
    fn nix_develop_wrap_argv_prefixes_develop_and_forwards_run_argv() {
        let run_argv = nix_run_args("./fixtures/basic-apps", "hello", &["one".to_owned()]);
        assert_eq!(
            nix_develop_wrap_run_args(
                "/nix/bin/nix",
                "./fixtures/basic-apps",
                "default",
                &run_argv
            ),
            vec![
                "develop".to_owned(),
                "./fixtures/basic-apps#default".to_owned(),
                "-c".to_owned(),
                "/nix/bin/nix".to_owned(),
                "run".to_owned(),
                "./fixtures/basic-apps#hello".to_owned(),
                "--".to_owned(),
                "one".to_owned(),
            ]
        );
    }

    #[test]
    fn nix_run_argv_preserves_forwarded_dashes() {
        assert_eq!(
            nix_run_args(".", "fmt", &["--".to_owned(), "--check".to_owned()]),
            vec![
                "run".to_owned(),
                ".#fmt".to_owned(),
                "--".to_owned(),
                "--".to_owned(),
                "--check".to_owned(),
            ]
        );
    }

    #[test]
    fn nix_build_and_check_argv_match_native_ops() {
        assert_eq!(
            nix_build_args(&package_installable(".", "aarch64-darwin", "nxr")),
            vec![
                "build".to_owned(),
                ".#packages.aarch64-darwin.nxr".to_owned()
            ]
        );
        assert_eq!(
            nix_flake_check_args("./fixtures/basic-apps"),
            vec![
                "flake".to_owned(),
                "check".to_owned(),
                "./fixtures/basic-apps".to_owned()
            ]
        );
        assert_eq!(
            nix_build_args(&check_installable(".", "x86_64-linux", "fmt")),
            vec!["build".to_owned(), ".#checks.x86_64-linux.fmt".to_owned()]
        );
    }

    #[test]
    fn nix_fmt_argv_formats_paths_or_cwd() {
        assert_eq!(nix_fmt_args(&[] as &[&str]), vec!["fmt".to_owned()]);
        assert_eq!(
            nix_fmt_args(&["flake.nix", "nix/"]),
            vec!["fmt".to_owned(), "flake.nix".to_owned(), "nix/".to_owned()]
        );
    }

    #[test]
    fn attr_installable_and_explicit_token_detection() {
        assert_eq!(
            attr_installable(".", "packages.x86_64-linux.nxr"),
            ".#packages.x86_64-linux.nxr"
        );
        assert!(token_is_explicit_installable(".#packages.x86_64-linux.nxr"));
        assert!(token_is_explicit_installable("github:foo/bar#default"));
        assert!(!token_is_explicit_installable("marker"));
    }

    #[test]
    fn nix_build_no_link_print_out_paths_argv() {
        assert_eq!(
            nix_build_no_link_print_out_paths_args("/nix/store/abc-hello"),
            vec![
                "build".to_owned(),
                "--no-link".to_owned(),
                "--print-out-paths".to_owned(),
                "/nix/store/abc-hello".to_owned(),
            ]
        );
    }

    #[test]
    fn flake_app_program_eval_argv() {
        assert_eq!(
            flake_app_program_eval_args(".", "aarch64-darwin", "hello"),
            vec![
                "eval".to_owned(),
                "--raw".to_owned(),
                ".#apps.aarch64-darwin.hello.program".to_owned(),
            ]
        );
    }

    #[test]
    fn nix_develop_argv_defaults_or_names_shell() {
        assert_eq!(
            nix_develop_args(".", None),
            vec!["develop".to_owned(), ".".to_owned()]
        );
        assert_eq!(
            nix_develop_args("./fixtures/named-dev-shells", Some("backend")),
            vec![
                "develop".to_owned(),
                "./fixtures/named-dev-shells#backend".to_owned()
            ]
        );
    }

    #[test]
    fn print_dev_env_argv_includes_json_flag_and_installable() {
        assert_eq!(
            print_dev_env_args(".", "default"),
            vec![
                "print-dev-env".to_owned(),
                ".#default".to_owned(),
                "--json".to_owned(),
            ]
        );
        assert_eq!(
            print_dev_env_args("./fixtures/named-dev-shells", "backend"),
            vec![
                "print-dev-env".to_owned(),
                "./fixtures/named-dev-shells#backend".to_owned(),
                "--json".to_owned(),
            ]
        );
    }
}
