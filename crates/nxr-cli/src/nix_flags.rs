//! Build capability-aware Nix argv flags from CLI globals.
//!
//! `--offline` and `--accept-flake-config` are [`nxr_nix::FlagPolicy::RequiredByUser`]:
//! unsupported capabilities surface as [`nxr_nix::NixError::UnsupportedOptionalFlag`].

use nxr_nix::OptionalNixFlags;

use crate::cli::Cli;

/// Parse `KEY=VAL` for `--nix-option`.
///
/// # Errors
///
/// Returns a usage message when the value is not `KEY=VAL`.
pub fn parse_nix_option(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VAL, got `{raw}`"))?;
    if key.is_empty() {
        return Err(format!("expected KEY=VAL, got `{raw}`"));
    }
    Ok((key.to_owned(), value.to_owned()))
}

/// Build optional Nix flags from global CLI options.
///
/// # Errors
///
/// Returns a usage message when `--nix-option` values are malformed.
pub fn nix_flags_from_cli(cli: &Cli) -> Result<OptionalNixFlags, String> {
    let nix_options = cli
        .nix_option
        .iter()
        .map(|raw| parse_nix_option(raw))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OptionalNixFlags {
        offline: cli.offline,
        accept_flake_config: cli.accept_flake_config,
        no_write_lock_file: false,
        json_log_format: false,
        nix_options,
        extra_argv: cli.nix_arg.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_nix_option;

    #[test]
    fn parse_nix_option_accepts_key_val_pairs() {
        assert_eq!(
            parse_nix_option("warn-dirty=false").expect("parse"),
            ("warn-dirty".to_owned(), "false".to_owned())
        );
    }

    #[test]
    fn parse_nix_option_rejects_missing_equals() {
        assert!(parse_nix_option("warn-dirty").is_err());
        assert!(parse_nix_option("=false").is_err());
    }
}
