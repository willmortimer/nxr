//! `nxr cache` subcommands.

use std::io::{self, Write};

use nxr_completion::{clear_discovery_cache, discovery_cache_status};
use nxr_core::diagnostics::exit;
use nxr_nix::{capability_cache_status, clear_capability_cache};
use serde::Serialize;

use crate::runner_output::RunnerOutput;

/// Errors while managing the discovery cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl CacheError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) | Self::Json(_) => exit::EVALUATION,
        }
    }
}

#[derive(Serialize)]
struct CacheClearJson {
    discovery_removed: usize,
    capabilities_removed: usize,
}

#[derive(Serialize)]
struct CacheStatusSection {
    path: String,
    entries: usize,
    total_bytes: u64,
}

#[derive(Serialize)]
struct CacheStatusJson {
    discovery: CacheStatusSection,
    capabilities: CacheStatusSection,
}

/// Remove all discovery cache entries.
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn clear(json: bool, runner: RunnerOutput) -> Result<(), CacheError> {
    let discovery_removed = clear_discovery_cache()?;
    let capabilities_removed = clear_capability_cache()?;
    if json {
        let payload = CacheClearJson {
            discovery_removed,
            capabilities_removed,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        runner
            .info(format!(
                "removed {discovery_removed} discovery cache entr{} and {capabilities_removed} capability cache entr{}",
                if discovery_removed == 1 { "y" } else { "ies" },
                if capabilities_removed == 1 { "y" } else { "ies" },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}

/// Print discovery cache location and size.
///
/// # Errors
///
/// Returns [`CacheError`] when the cache directory cannot be read or output fails.
pub fn status(json: bool, mut runner: RunnerOutput) -> Result<(), CacheError> {
    let discovery = discovery_cache_status()?;
    let capabilities = capability_cache_status()?;
    if json {
        let payload = CacheStatusJson {
            discovery: CacheStatusSection {
                path: discovery.path,
                entries: discovery.entries,
                total_bytes: discovery.total_bytes,
            },
            capabilities: CacheStatusSection {
                path: capabilities.path,
                entries: capabilities.entries,
                total_bytes: capabilities.total_bytes,
            },
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        render_status_section(
            &mut runner,
            "discovery",
            &discovery.path,
            discovery.entries,
            discovery.total_bytes,
        )?;
        render_status_section(
            &mut runner,
            "capability",
            &capabilities.path,
            capabilities.entries,
            capabilities.total_bytes,
        )?;
    }
    Ok(())
}

fn render_status_section(
    runner: &mut RunnerOutput,
    label: &str,
    path: &str,
    entries: usize,
    total_bytes: u64,
) -> Result<(), CacheError> {
    if path.is_empty() {
        runner
            .info(format!("{label} cache unavailable on this host"))
            .map_err(CacheError::Io)?;
    } else {
        runner
            .info(format!(
                "{label} cache: {path} ({entries} entr{}, {total_bytes} bytes)",
                if entries == 1 { "y" } else { "ies" },
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}
