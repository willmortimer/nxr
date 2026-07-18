//! CLI subcommands.

pub mod list;

use nxr_core::diagnostics::exit;

/// User-facing error for reserved commands that are not implemented in Phase 1.
#[derive(Debug, thiserror::Error)]
#[error("nxr {command} is not implemented yet")]
pub struct UnimplementedCommandError {
    pub command: &'static str,
}

impl UnimplementedCommandError {
    #[must_use]
    pub const fn exit_code() -> i32 {
        exit::USAGE
    }
}
