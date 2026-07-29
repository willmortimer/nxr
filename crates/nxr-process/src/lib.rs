//! Foreground execution, signals, and (later) supervision.

pub mod chunk_coalesce;
pub mod deadline;
pub mod foreground;
pub mod pipe_multiplex;
pub mod session;
pub mod signals;
pub mod supervisor;

#[cfg(all(test, unix))]
mod test_unix;

pub use chunk_coalesce::{ChunkCoalescer, CoalesceLimits};
pub use deadline::DeadlineQueue;
pub use foreground::{run, run_in, run_in_with_stderr, run_in_with_stderr_lines};
pub use pipe_multiplex::{PipeChunk, PipeMultiplexer, PipeStream};
pub use session::{ChildSession, SpawnStdio, spawn_in, spawn_in_with};
pub use signals::{InterruptFlags, exit_code_from_status};
pub use supervisor::Supervisor;
