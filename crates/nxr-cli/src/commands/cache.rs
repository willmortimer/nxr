//! `nxr cache` subcommands.

use std::io::{self, Write};

use nxr_completion::{clear_discovery_cache, discovery_cache_status};
use nxr_core::diagnostics::exit;
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
    removed: usize,
}

#[derive(Serialize)]
struct CacheStatusJson {
    path: String,
    entries: usize,
    total_bytes: u64,
}

/// Remove all discovery cache entries.
///
/// # Errors
///
/// Returns [`CacheError`] when cache files cannot be removed or output fails.
pub fn clear(json: bool, runner: RunnerOutput) -> Result<(), CacheError> {
    let removed = clear_discovery_cache()?;
    if json {
        let payload = CacheClearJson { removed };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else {
        runner
            .info(format!(
                "removed {removed} discovery cache entr{}",
                if removed == 1 { "y" } else { "ies" }
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
pub fn status(json: bool, runner: RunnerOutput) -> Result<(), CacheError> {
    let status = discovery_cache_status()?;
    if json {
        let payload = CacheStatusJson {
            path: status.path,
            entries: status.entries,
            total_bytes: status.total_bytes,
        };
        let rendered = serde_json::to_string_pretty(&payload)?;
        writeln!(io::stdout().lock(), "{rendered}")?;
    } else if status.path.is_empty() {
        runner
            .info("discovery cache unavailable on this host")
            .map_err(CacheError::Io)?;
    } else {
        runner
            .info(format!(
                "discovery cache: {} ({} entr{}, {} bytes)",
                status.path,
                status.entries,
                if status.entries == 1 { "y" } else { "ies" },
                status.total_bytes
            ))
            .map_err(CacheError::Io)?;
    }
    Ok(())
}
