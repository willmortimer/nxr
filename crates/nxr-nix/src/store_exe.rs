//! Realise flake-app store executables for optional direct spawn (ADR-0153).

use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};

use crate::NixError;
use crate::capabilities::{NixFailureKind, OptionalNixFlags, run_nix};
use crate::command::{flake_app_program_eval_args, nix_build_no_link_print_out_paths_args};
use nxr_core::{store_exe_path_usable, store_output_root_for_program};

/// Realised app program plus its store output root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RealisedAppProgram {
    pub program: Utf8PathBuf,
    pub store_output: Utf8PathBuf,
}

/// Evaluate `apps.<system>.<app>.program`, realise the output if needed, and
/// return the executable path.
///
/// Uses `nix eval --raw` then, when the path is not yet a usable file,
/// `nix build --no-link --print-out-paths` on the store output root.
///
/// # Errors
///
/// Returns [`NixError`] when eval/build fails or the program path is unusable.
pub fn realise_flake_app_program(
    nix: &Utf8Path,
    flake_ref: &str,
    system: &str,
    app_name: &str,
    nix_flags: &OptionalNixFlags,
    cwd: Option<&Path>,
) -> Result<RealisedAppProgram, NixError> {
    let eval_base = flake_app_program_eval_args(flake_ref, system, app_name);
    let eval_args = apply_flags(nix, eval_base, nix_flags)?;
    let stdout = run_nix_in(nix, &eval_args, NixFailureKind::Evaluation, cwd)?;
    let program = String::from_utf8(stdout)
        .map_err(|_| NixError::InvalidVersionOutput)?
        .trim()
        .to_owned();
    if program.is_empty() {
        return Err(NixError::CommandFailed {
            nix: nix.to_path_buf(),
            args: eval_args,
            status: Some(1),
            stderr: "empty app program path from nix eval".to_owned(),
            kind: NixFailureKind::Evaluation,
        });
    }

    let Some(store_output) = store_output_root_for_program(&program) else {
        return Err(NixError::CommandFailed {
            nix: nix.to_path_buf(),
            args: eval_args,
            status: Some(1),
            stderr: format!("app program is not a /nix/store path: {program}"),
            kind: NixFailureKind::Evaluation,
        });
    };

    let program_path = Utf8PathBuf::from(program);
    if !store_exe_path_usable(program_path.as_std_path()) {
        let build_base = nix_build_no_link_print_out_paths_args(&store_output);
        let build_args = apply_flags(nix, build_base, nix_flags)?;
        let _ = run_nix_in(nix, &build_args, NixFailureKind::Evaluation, cwd)?;
    }

    if !store_exe_path_usable(program_path.as_std_path()) {
        return Err(NixError::CommandFailed {
            nix: nix.to_path_buf(),
            args: eval_args,
            status: Some(1),
            stderr: format!("realised app program is not executable: {program_path}"),
            kind: NixFailureKind::Evaluation,
        });
    }

    Ok(RealisedAppProgram {
        program: program_path,
        store_output: Utf8PathBuf::from(store_output),
    })
}

fn apply_flags(
    nix: &Utf8Path,
    base: Vec<String>,
    requested: &OptionalNixFlags,
) -> Result<Vec<String>, NixError> {
    // Floor capabilities: only apply flags the caller already validated elsewhere
    // when possible. For store-exe we best-effort prepend optional flags that do
    // not require probing (offline / accept-flake-config need support).
    let mut args = base;
    if requested.json_log_format {
        args.insert(1, "--log-format".to_owned());
        args.insert(2, "internal-json".to_owned());
    }
    if requested.no_write_lock_file {
        args.push("--no-write-lock-file".to_owned());
    }
    if requested.offline {
        args.push("--offline".to_owned());
    }
    if requested.accept_flake_config {
        args.push("--accept-flake-config".to_owned());
    }
    for (key, value) in &requested.nix_options {
        args.push("--option".to_owned());
        args.push(key.clone());
        args.push(value.clone());
    }
    args.extend(requested.extra_argv.iter().cloned());
    let _ = nix;
    Ok(args)
}

fn run_nix_in(
    nix: &Utf8Path,
    args: &[String],
    failure_kind: NixFailureKind,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, NixError> {
    if cwd.is_none() {
        return run_nix(nix, args, failure_kind);
    }
    nxr_core::record_nix_spawn();
    let mut command = std::process::Command::new(nix.as_std_path());
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let output = command.output().map_err(|source| NixError::SpawnFailed {
        nix: nix.to_path_buf(),
        source,
    })?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Err(NixError::CommandFailed {
        nix: nix.to_path_buf(),
        args: args.to_vec(),
        status: output.status.code(),
        stderr,
        kind: failure_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_flags_appends_optional_switches() {
        let nix = Utf8Path::new("/nix/bin/nix");
        let flags = OptionalNixFlags {
            offline: true,
            no_write_lock_file: true,
            accept_flake_config: false,
            json_log_format: false,
            nix_options: vec![],
            extra_argv: vec![],
        };
        let args = apply_flags(
            nix,
            flake_app_program_eval_args(".", "aarch64-darwin", "hello"),
            &flags,
        )
        .expect("flags");
        assert!(args.contains(&"--offline".to_owned()));
        assert!(args.contains(&"--no-write-lock-file".to_owned()));
        assert_eq!(args[0], "eval");
    }
}
