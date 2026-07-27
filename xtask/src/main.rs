//! Repository maintenance tasks for nxr.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("man") => generate_man(args.next()),
        Some("cli-ref") => generate_cli_ref(args.next()),
        Some(other) => {
            eprintln!("unknown xtask: {other}");
            eprintln!("usage: cargo xtask man [OUT]");
            eprintln!("       cargo xtask cli-ref [OUT]");
            ExitCode::from(2)
        }
        None => {
            eprintln!("usage: cargo xtask man [OUT]");
            eprintln!("       cargo xtask cli-ref [OUT]");
            ExitCode::from(2)
        }
    }
}

fn generate_man(out: Option<String>) -> ExitCode {
    let out_path = out.map_or_else(|| PathBuf::from("nxr.1"), PathBuf::from);

    let output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["run", "-p", "nxr-cli", "--quiet", "--", "__manpage"])
        .output();

    let output = match output {
        Ok(output) => output,
        Err(error) => {
            eprintln!("failed to run nxr-cli: {error}");
            return ExitCode::from(1);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("nxr __manpage failed:\n{stderr}");
        return ExitCode::from(1);
    }

    if let Err(error) = fs::write(&out_path, &output.stdout) {
        eprintln!("failed to write {}: {error}", out_path.display());
        return ExitCode::from(1);
    }

    println!("wrote {}", out_path.display());
    ExitCode::SUCCESS
}

fn generate_cli_ref(out: Option<String>) -> ExitCode {
    let out_path = out.map_or_else(|| PathBuf::from("docs/CLI_GENERATED.md"), PathBuf::from);

    let help = match run_nxr_help(&["--help"]) {
        Ok(help) => help,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    let task_help = match run_nxr_help(&["task", "--help"]) {
        Ok(help) => help,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    let rendered = format!(
        "# Generated CLI reference\n\n\
This file is generated from Clap help output. Regenerate with:\n\n\
```bash\ncargo xtask cli-ref\n```\n\n\
<!-- BEGIN GENERATED -->\n\n\
## `nxr --help`\n\n\
```text\n{help}```\n\n\
## `nxr task --help`\n\n\
```text\n{task_help}```\n\n\
<!-- END GENERATED -->\n"
    );

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("failed to create {}: {error}", parent.display());
                return ExitCode::from(1);
            }
        }
    }

    if let Err(error) = fs::write(&out_path, rendered) {
        eprintln!("failed to write {}: {error}", out_path.display());
        return ExitCode::from(1);
    }

    println!("wrote {}", out_path.display());
    ExitCode::SUCCESS
}

fn run_nxr_help(args: &[&str]) -> Result<String, String> {
    let output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .arg("run")
        .arg("-p")
        .arg("nxr-cli")
        .arg("--quiet")
        .arg("--")
        .args(args)
        .output()
        .map_err(|error| format!("failed to run nxr-cli: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("nxr {} failed:\n{stderr}", args.join(" ")));
    }

    String::from_utf8(output.stdout).map_err(|error| format!("nxr help was not utf-8: {error}"))
}
