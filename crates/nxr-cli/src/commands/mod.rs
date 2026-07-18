//! CLI subcommands.

pub mod common;
pub mod complete;
pub mod completion;
pub mod doctor;
pub mod list;
pub mod manpage;
pub mod plan;
pub mod run;
pub mod select;

use nxr_core::diagnostics::exit;

/// User-facing error for reserved commands that are not implemented yet.
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
