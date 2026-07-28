//! Lean CLI entry paths that avoid Nix, discovery, and full dispatch.

use std::ffi::OsString;

use clap::ValueEnum;
use nxr_completion::{CompleteTarget, Shell};
use nxr_nix::OptionalNixFlags;

use crate::commands::{complete, completion, manpage};

/// Global flags that consume the next argv token as a value.
const VALUE_FLAGS: &[&str] = &[
    "-f",
    "--flake",
    "-C",
    "--cwd",
    "--nix",
    "--shell",
    "--color",
    "--log-format",
    "--output",
    "--events",
    "-j",
    "--jobs",
    "--debounce",
    "--keep-env",
    "--set-env",
    "--unset-env",
    "--report",
    "--nix-option",
    "--nix-arg",
];

/// Reserved top-level subcommands (not bare app names). Documented for shell helpers.
#[allow(dead_code)]
const RESERVED_SUBCOMMANDS: &[&str] = &[
    "list",
    "run",
    "script",
    "plan",
    "select",
    "doctor",
    "explain",
    "completion",
    "inspect",
    "task",
    "watch",
    "graph",
    "build",
    "check",
    "shell",
    "cache",
    "daemon",
    "history",
    "trust",
    "inventory",
    "up",
    "status",
    "logs",
    "down",
    "affected",
    "fmt",
    "envrc",
    "init",
    "migrate",
    "context",
    "in",
    "ci",
    "__complete",
    "__manpage",
    "help",
];

/// Attempt a lean startup path before full Clap parsing and dispatch.
///
/// Returns `Some(Ok(code))` or `Some(Err(message))` when this layer handled the
/// invocation; `None` when the full CLI should run.
pub fn try_run() -> Option<Result<i32, String>> {
    let argv: Vec<OsString> = std::env::args_os().collect();
    if is_top_level_version(&argv) {
        return Some(Ok(print_version()));
    }

    let subcommand = lean_subcommand(&argv)?;

    match subcommand.as_str() {
        "completion" => {
            let index = subcommand_index(&argv, "completion")?;
            let shell = argv.get(index + 1).and_then(|value| value.to_str())?;
            let shell = match Shell::from_str(shell, true) {
                Ok(shell) => shell,
                Err(error) => return Some(Err(error.to_string())),
            };
            if let Err(error) = completion::run(shell) {
                return Some(Err(error.to_string()));
            }
            Some(Ok(nxr_core::diagnostics::exit::SUCCESS))
        }
        "__manpage" => {
            if let Err(error) = manpage::run() {
                return Some(Err(error.to_string()));
            }
            Some(Ok(nxr_core::diagnostics::exit::SUCCESS))
        }
        "__complete" => {
            let index = subcommand_index(&argv, "__complete")?;
            let target = argv.get(index + 1).and_then(|value| value.to_str())?;
            let target = match CompleteTarget::from_str(target, true) {
                Ok(target) => target,
                Err(error) => return Some(Err(error.to_string())),
            };
            let (flake, nix, refresh_discovery) = parse_lean_globals(&argv);
            if let Err(error) = complete::run(
                target,
                flake.as_deref(),
                nix.as_deref(),
                refresh_discovery,
                &OptionalNixFlags::default(),
            ) {
                return Some(Err(error.to_string()));
            }
            Some(Ok(nxr_core::diagnostics::exit::SUCCESS))
        }
        _ => None,
    }
}

fn print_version() -> i32 {
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    nxr_core::diagnostics::exit::SUCCESS
}

fn is_top_level_version(argv: &[OsString]) -> bool {
    let mut index = 1usize;
    while index < argv.len() {
        let Some(arg) = argv[index].to_str() else {
            return false;
        };
        if arg == "--" {
            return false;
        }
        if arg == "-V" || arg == "--version" {
            return true;
        }
        if arg.starts_with('-') {
            if flag_takes_value(arg) {
                index += 1;
            }
            index += 1;
            continue;
        }
        return false;
    }
    false
}

fn lean_subcommand(argv: &[OsString]) -> Option<String> {
    let mut index = 1usize;
    while index < argv.len() {
        let arg = argv[index].to_str()?;
        if arg == "--" {
            return None;
        }
        if arg == "-V" || arg == "--version" {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            if flag_takes_value(arg) {
                index += 1;
            }
            index += 1;
            continue;
        }
        return Some(arg.to_owned());
    }
    None
}

fn subcommand_index(argv: &[OsString], name: &str) -> Option<usize> {
    argv.iter().position(|arg| arg.to_str() == Some(name))
}

fn flag_takes_value(flag: &str) -> bool {
    if VALUE_FLAGS.contains(&flag) {
        return true;
    }
    flag.starts_with("--keep-env=")
        || flag.starts_with("--set-env=")
        || flag.starts_with("--unset-env=")
        || flag.starts_with("--report=")
        || flag.starts_with("--nix-option=")
        || flag.starts_with("--nix-arg=")
        || flag.starts_with("--flake=")
        || flag.starts_with("--cwd=")
        || flag.starts_with("--nix=")
        || flag.starts_with("--shell=")
}

fn parse_lean_globals(argv: &[OsString]) -> (Option<String>, Option<String>, bool) {
    let mut flake = None;
    let mut nix = None;
    let mut refresh_discovery = false;
    let mut index = 1usize;
    while index < argv.len() {
        let Some(arg) = argv[index].to_str() else {
            break;
        };
        match arg {
            "-f" | "--flake" => {
                index += 1;
                flake = argv
                    .get(index)
                    .and_then(|value| value.to_str())
                    .map(str::to_owned);
            }
            "--nix" => {
                index += 1;
                nix = argv
                    .get(index)
                    .and_then(|value| value.to_str())
                    .map(str::to_owned);
            }
            "--refresh-discovery" => refresh_discovery = true,
            _ => {
                if let Some(value) = arg.strip_prefix("--flake=") {
                    flake = Some(value.to_owned());
                } else if let Some(value) = arg.strip_prefix("--nix=") {
                    nix = Some(value.to_owned());
                }
            }
        }
        index += 1;
    }
    (flake, nix, refresh_discovery)
}

#[cfg(test)]
mod tests {
    use super::{RESERVED_SUBCOMMANDS, is_top_level_version};

    fn argv(parts: &[&str]) -> Vec<std::ffi::OsString> {
        parts.iter().map(|part| (*part).into()).collect()
    }

    #[test]
    fn top_level_version_detects_global_flags() {
        assert!(is_top_level_version(&argv(&["nxr", "--version"])));
        assert!(is_top_level_version(&argv(&["nxr", "-V"])));
        assert!(is_top_level_version(&argv(&["nxr", "--json", "--version"])));
    }

    #[test]
    fn top_level_version_rejects_subcommand_forms() {
        assert!(!is_top_level_version(&argv(&["nxr", "list", "--version"])));
        assert!(!is_top_level_version(&argv(&[
            "nxr",
            "run",
            "hello",
            "--version"
        ])));
        assert!(!is_top_level_version(&argv(&["nxr", "hello", "--version"])));
    }

    #[test]
    fn reserved_subcommands_include_hidden_protocol() {
        assert!(RESERVED_SUBCOMMANDS.contains(&"__complete"));
        assert!(RESERVED_SUBCOMMANDS.contains(&"completion"));
    }
}
