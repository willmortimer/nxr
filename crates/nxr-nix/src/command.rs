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

#[cfg(test)]
mod tests {
    use super::{current_system_args, flake_show_args, nix_run_args};

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
}
