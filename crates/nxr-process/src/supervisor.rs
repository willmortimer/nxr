//! Multi-child supervision for V2.
//!
//! V1 owns a single foreground child (see [`crate::foreground`]). Parallel
//! process groups, escalation, and a shared supervisor land in V2 (ADR-0108).

/// Placeholder for the V2 multi-child supervisor.
///
/// Not constructed in V1; kept so the module layout matches the crate map.
#[derive(Debug)]
pub struct Supervisor {
    _private: (),
}
