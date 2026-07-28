//! Workspace action CAS restore/save at task execution time.

use std::io;

use camino::Utf8Path;
use nxr_core::ActionTier;
use nxr_core::cas::{
    CacheExplain, CacheLookupExplain, CasLookup, lookup_outputs, restore_outputs, save_outputs,
};
use nxr_task::WorkspaceCachePlan;

use crate::commands::common::PreparedTaskNode;

/// Describe cache behavior for explain / dry-run.
#[must_use]
pub fn explain_workspace_cache(prepared: &PreparedTaskNode) -> CacheExplain {
    let Some(plan) = prepared.workspace_cache.as_ref() else {
        return CacheExplain {
            tier: ActionTier::DerivationBacked,
            cache_enabled: false,
            action_key: None,
            lookup: CacheLookupExplain::Skipped {
                reason: "no workspace cache plan".to_owned(),
            },
            key_components: Default::default(),
        };
    };
    build_explain(plan)
}

fn build_explain(plan: &WorkspaceCachePlan) -> CacheExplain {
    if !plan.cache_enabled {
        return CacheExplain {
            tier: plan.tier,
            cache_enabled: false,
            action_key: plan.action_key.clone(),
            lookup: CacheLookupExplain::Skipped {
                reason: "cache.mode disabled or no outputs declared".to_owned(),
            },
            key_components: plan.key_components.clone(),
        };
    }
    if !plan.restore {
        return CacheExplain {
            tier: plan.tier,
            cache_enabled: true,
            action_key: plan.action_key.clone(),
            lookup: CacheLookupExplain::Skipped {
                reason: "cache.restore=false".to_owned(),
            },
            key_components: plan.key_components.clone(),
        };
    }
    let Some(action_key) = plan.action_key.as_ref() else {
        return CacheExplain {
            tier: plan.tier,
            cache_enabled: true,
            action_key: None,
            lookup: CacheLookupExplain::Skipped {
                reason: "action key unavailable".to_owned(),
            },
            key_components: plan.key_components.clone(),
        };
    };
    let lookup = match lookup_outputs(action_key, &plan.outputs) {
        Ok(CasLookup::Hit) => CacheLookupExplain::Hit,
        Ok(CasLookup::Miss { reason }) => CacheLookupExplain::Miss { reason },
        Err(error) => CacheLookupExplain::Miss {
            reason: error.to_string(),
        },
    };
    CacheExplain {
        tier: plan.tier,
        cache_enabled: true,
        action_key: Some(action_key.clone()),
        lookup,
        key_components: plan.key_components.clone(),
    }
}

/// Attempt to restore workspace outputs from CAS before spawn.
///
/// Returns `Some(Hit)` when the node can be skipped.
///
/// # Errors
///
/// Returns [`io::Error`] when restore I/O fails unexpectedly.
pub fn try_workspace_cache_restore(
    prepared: &PreparedTaskNode,
    flake_root: &Utf8Path,
) -> io::Result<Option<CasLookup>> {
    let Some(plan) = prepared.workspace_cache.as_ref() else {
        return Ok(None);
    };
    if plan.tier == ActionTier::DerivationBacked || !plan.cache_enabled || !plan.restore {
        return Ok(None);
    }
    let Some(action_key) = plan.action_key.as_ref() else {
        return Ok(None);
    };
    let lookup = restore_outputs(flake_root, action_key, &plan.outputs)?;
    if matches!(lookup, CasLookup::Hit) {
        return Ok(Some(lookup));
    }
    Ok(None)
}

/// Save workspace outputs after a successful node run.
///
/// # Errors
///
/// Returns [`io::Error`] when save I/O fails.
pub fn save_workspace_cache(prepared: &PreparedTaskNode, flake_root: &Utf8Path) -> io::Result<()> {
    let Some(plan) = prepared.workspace_cache.as_ref() else {
        return Ok(());
    };
    if plan.tier == ActionTier::DerivationBacked || !plan.cache_enabled || !plan.save {
        return Ok(());
    }
    let Some(action_key) = plan.action_key.as_ref() else {
        return Ok(());
    };
    save_outputs(flake_root, action_key, &plan.outputs)
}
