//! Foreground execution, signals, and (later) supervision.

pub mod foreground;
pub mod signals;
pub mod supervisor;

pub use foreground::{run, run_in};
pub use signals::exit_code_from_status;
