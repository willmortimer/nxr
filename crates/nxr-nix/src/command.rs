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

#[cfg(test)]
mod tests {
    use super::{current_system_args, flake_show_args};

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
}
