//! Explicit catalog retention. Task execution does not call this.

use std::path::PathBuf;

use nxr_task::artifact::{catalog_directory, preview_retention, retain_catalog};

use crate::runner_output::RunnerOutput;

pub fn retain(
    catalog: Option<PathBuf>,
    quota: usize,
    dry_run: bool,
    runner: RunnerOutput,
) -> Result<(), String> {
    let catalog = match catalog {
        Some(path) => path,
        None => catalog_directory().ok_or_else(|| "catalog directory is not set".to_owned())?,
    };
    let preview = preview_retention(&catalog, quota)?;
    runner
        .info(format!(
            "pins {}",
            if preview.pins.is_empty() {
                "none".into()
            } else {
                preview.pins.join(",")
            }
        ))
        .map_err(|err| err.to_string())?;
    runner
        .info(format!(
            "quarantine {}",
            if preview.quarantine.is_empty() {
                "none".into()
            } else {
                preview.quarantine.join(",")
            }
        ))
        .map_err(|err| err.to_string())?;
    runner
        .info(format!(
            "expire {}",
            if preview.would_expire.is_empty() {
                "none".into()
            } else {
                preview.would_expire.join(",")
            }
        ))
        .map_err(|err| err.to_string())?;
    if dry_run {
        return Ok(());
    }
    retain_catalog(&catalog, quota)?;
    runner
        .info("catalog retain finished")
        .map_err(|err| err.to_string())?;
    Ok(())
}
