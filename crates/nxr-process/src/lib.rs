//! Foreground execution, signals, and (later) supervision.

pub mod foreground;
pub mod signals;
pub mod supervisor;

pub use foreground::run;
pub use signals::exit_code_from_status;
