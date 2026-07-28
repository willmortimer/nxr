//! Store path query batching hooks (Wave 8b).
//!
//! When [`crate::strategy::DiscoveryEvalPlan::batched_store_queries`] is true,
//! callers should aggregate `nix path-info --json` requests instead of spawning
//! one subprocess per store path. Full batching lands in perf-8b.

use crate::strategy::DiscoveryEvalPlan;

/// Whether store path resolution should prefer batched `nix path-info --json`.
#[must_use]
pub fn prefer_batched_store_queries(plan: &DiscoveryEvalPlan) -> bool {
    plan.batched_store_queries
}
