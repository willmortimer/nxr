//! Foreground execution, signals, and (later) supervision.

pub mod foreground;
pub mod session;
pub mod signals;
pub mod supervisor;

pub use foreground::{run, run_in};
pub use session::{ChildSession, spawn_in};
pub use signals::{InterruptFlags, exit_code_from_status};
