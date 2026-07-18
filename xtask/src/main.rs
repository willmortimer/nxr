//! Repository maintenance tasks for nxr.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("man") => generate_man(args.next()),
        Some(other) => {
            eprintln!("unknown xtask: {other}");
            eprintln!("usage: cargo xtask man [OUT]");
            ExitCode::from(2)
        }
        None => {
            eprintln!("usage: cargo xtask man [OUT]");
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
