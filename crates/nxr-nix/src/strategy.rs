//! Nix discovery/evaluation strategy selection (ADR-0165).
//!
//! Chooses the fastest safe cold-discovery path from negotiated capabilities
//! and Determinate probes. Callers fall back to the compatibility path on miss
//! or error.

use serde::Serialize;

use crate::determinate::{
    DeterminatePerformanceFeatures, LazyTreesState, distribution_from_version_banner,
    probe_performance_features,
};

/// Environment variable forcing coalesced discovery (integration tests).
pub const FORCE_COALESCED_DISCOVERY_ENV: &str = "NXR_FORCE_COALESCED_DISCOVERY";

/// Kill-switch forcing the compatibility discovery path (`flake show` + separate evals).
pub const FORCE_COMPATIBILITY_STRATEGY_ENV: &str = "NXR_EVAL_STRATEGY";

/// Selected cold-discovery evaluation strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryEvalStrategy {
    /// Single coalesced `nix eval` when Determinate parallel eval is available.
    CoalescedParallelEval,
    /// Metadata-oriented separate evals when lazy trees are enabled or assumed.
    LazyTreesCompatible,
    /// Upstream/Lix compatibility: `flake show` plus targeted evals.
    Compatibility,
}

/// Planned discovery/evaluation path for one cold workspace load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryEvalPlan {
    pub strategy: DiscoveryEvalStrategy,
    pub features: DeterminatePerformanceFeatures,
    /// Attempt coalesced cold discovery (`nix eval --json --expr …`).
    pub use_coalesced_discovery: bool,
    /// Wave 8b: prefer batched `nix path-info --json` for store lookups.
    pub batched_store_queries: bool,
    /// Wave 8c: optional eval worker may accelerate repeated eval (not implemented).
    pub eval_worker_eligible: bool,
}

/// Plan the fastest safe discovery strategy from Nix probes.
#[must_use]
pub fn plan_discovery_eval(
    version_banner: &str,
    config_json: Option<&str>,
    load_tasks: bool,
) -> DiscoveryEvalPlan {
    plan_discovery_eval_with_overrides(
        version_banner,
        config_json,
        load_tasks,
        std::env::var_os(FORCE_COALESCED_DISCOVERY_ENV).is_some(),
        compatibility_forced(),
    )
}

pub(crate) fn plan_discovery_eval_with_overrides(
    version_banner: &str,
    config_json: Option<&str>,
    _load_tasks: bool,
    force_coalesced: bool,
    force_compatibility: bool,
) -> DiscoveryEvalPlan {
    if force_compatibility {
        let distribution = distribution_from_version_banner(version_banner);
        let features = probe_performance_features(&distribution, config_json);
        let batched_store_queries = batched_store_queries_from(&features);
        return DiscoveryEvalPlan {
            strategy: DiscoveryEvalStrategy::Compatibility,
            features,
            use_coalesced_discovery: false,
            batched_store_queries,
            eval_worker_eligible: eval_worker_eligible_from(&distribution),
        };
    }

    if force_coalesced {
        let distribution = distribution_from_version_banner(version_banner);
        let features = probe_performance_features(&distribution, config_json);
        let batched_store_queries = batched_store_queries_from(&features);
        return DiscoveryEvalPlan {
            strategy: DiscoveryEvalStrategy::CoalescedParallelEval,
            features,
            use_coalesced_discovery: true,
            batched_store_queries,
            eval_worker_eligible: eval_worker_eligible_from(&distribution),
        };
    }

    let distribution = distribution_from_version_banner(version_banner);
    let features = probe_performance_features(&distribution, config_json);
    let strategy = select_strategy(&distribution, &features);
    let use_coalesced_discovery = strategy == DiscoveryEvalStrategy::CoalescedParallelEval;
    let batched_store_queries = batched_store_queries_from(&features);

    DiscoveryEvalPlan {
        strategy,
        features,
        use_coalesced_discovery,
        batched_store_queries,
        eval_worker_eligible: eval_worker_eligible_from(&distribution),
    }
}

fn compatibility_forced() -> bool {
    compatibility_forced_from(
        std::env::var(FORCE_COMPATIBILITY_STRATEGY_ENV)
            .ok()
            .as_deref(),
    )
}

fn compatibility_forced_from(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("compatibility" | "compat")
    )
}

fn select_strategy(
    distribution: &crate::capabilities::NixDistribution,
    features: &DeterminatePerformanceFeatures,
) -> DiscoveryEvalStrategy {
    if features.parallel_eval_available {
        return DiscoveryEvalStrategy::CoalescedParallelEval;
    }

    if lazy_trees_assumed(features, distribution) {
        return DiscoveryEvalStrategy::LazyTreesCompatible;
    }

    DiscoveryEvalStrategy::Compatibility
}

fn lazy_trees_assumed(
    features: &DeterminatePerformanceFeatures,
    distribution: &crate::capabilities::NixDistribution,
) -> bool {
    match features.lazy_trees {
        LazyTreesState::Enabled => true,
        LazyTreesState::Disabled => false,
        LazyTreesState::Unconfigured => distribution.is_determinate(),
    }
}

fn batched_store_queries_from(features: &DeterminatePerformanceFeatures) -> bool {
    features.lazy_trees != LazyTreesState::Disabled
}

fn eval_worker_eligible_from(distribution: &crate::capabilities::NixDistribution) -> bool {
    // Wave 8c hook: eligibility only; worker remains off by default.
    distribution.is_determinate()
}

#[cfg(test)]
mod tests {
    use super::{
        DiscoveryEvalStrategy, compatibility_forced_from, plan_discovery_eval,
        plan_discovery_eval_with_overrides,
    };
    use crate::capabilities::NixDistribution;
    use crate::determinate::{LazyTreesState, probe_performance_features};

    const DETERMINATE_BANNER: &str = "nix (Determinate Nix 3.21.7) 2.34.8\n";
    const UPSTREAM_BANNER: &str = "nix (Nix) 2.34.7\n";

    #[test]
    fn determinate_selects_coalesced_parallel_eval() {
        let plan = plan_discovery_eval(DETERMINATE_BANNER, None, true);
        assert_eq!(plan.strategy, DiscoveryEvalStrategy::CoalescedParallelEval);
        assert!(plan.use_coalesced_discovery);
        assert!(plan.eval_worker_eligible);
    }

    #[test]
    fn upstream_selects_compatibility_without_lazy_trees() {
        let plan = plan_discovery_eval(UPSTREAM_BANNER, None, false);
        assert_eq!(plan.strategy, DiscoveryEvalStrategy::Compatibility);
        assert!(!plan.use_coalesced_discovery);
        assert!(!plan.eval_worker_eligible);
    }

    #[test]
    fn upstream_with_lazy_trees_selects_lazy_trees_compatible() {
        let config = r#"{"lazy-trees": {"value": true}}"#;
        let plan = plan_discovery_eval(UPSTREAM_BANNER, Some(config), false);
        assert_eq!(plan.strategy, DiscoveryEvalStrategy::LazyTreesCompatible);
        assert!(!plan.use_coalesced_discovery);
        assert!(plan.batched_store_queries);
    }

    #[test]
    fn determinate_lazy_trees_disabled_stays_coalesced() {
        let config = r#"{"lazy-trees": {"value": false}}"#;
        let plan = plan_discovery_eval(DETERMINATE_BANNER, Some(config), true);
        assert_eq!(plan.strategy, DiscoveryEvalStrategy::CoalescedParallelEval);
        assert!(plan.use_coalesced_discovery);
        assert!(!plan.batched_store_queries);
    }

    #[test]
    fn force_coalesced_override_selects_coalesced_on_upstream() {
        let plan = plan_discovery_eval_with_overrides(UPSTREAM_BANNER, None, true, true, false);
        assert_eq!(plan.strategy, DiscoveryEvalStrategy::CoalescedParallelEval);
        assert!(plan.use_coalesced_discovery);
    }

    #[test]
    fn force_compatibility_override_on_determinate() {
        let plan = plan_discovery_eval_with_overrides(DETERMINATE_BANNER, None, true, false, true);
        assert_eq!(plan.strategy, DiscoveryEvalStrategy::Compatibility);
        assert!(!plan.use_coalesced_discovery);
    }

    #[test]
    fn compatibility_forced_from_accepts_aliases() {
        assert!(compatibility_forced_from(Some("compatibility")));
        assert!(compatibility_forced_from(Some(" compat ")));
        assert!(!compatibility_forced_from(Some("coalesced")));
    }

    #[test]
    fn probe_features_align_with_strategy_inputs() {
        let distribution = NixDistribution::Determinate {
            product_version: Some("3.0.0".to_owned()),
        };
        let features = probe_performance_features(&distribution, None);
        assert!(features.parallel_eval_available);
        assert_eq!(features.lazy_trees, LazyTreesState::Unconfigured);
    }
}
