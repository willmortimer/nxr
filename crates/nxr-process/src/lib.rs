//! Foreground execution, signals, and (later) supervision.

pub mod foreground;
pub mod session;
pub mod signals;
pub mod supervisor;

pub use foreground::{run, run_in};
pub use session::{ChildSession, SpawnStdio, spawn_in, spawn_in_with};
pub use signals::{InterruptFlags, exit_code_from_status};
pub use supervisor::Supervisor;
