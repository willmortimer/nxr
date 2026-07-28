//! Experimental optional Nix eval-result worker retained by `nxrd` ([ADR-0168]).
//!
//! Off by default (`NXR_EVAL_WORKER=1` to opt in). Retains narrowly typed
//! metadata / tasks / list JSON across CLI invocations after `eval.prepare`.
//! Never an evaluation authority or secret store — clients fall back to
//! subprocess `nix eval` on any doubt.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Opt-in environment variable (`1` / `true` / `on` / `yes`).
pub const EVAL_WORKER_ENV: &str = "NXR_EVAL_WORKER";

/// Maximum retained eval JSON entries.
pub const MAX_EVAL_ENTRIES: usize = 128;

/// Maximum JSON string bytes accepted by `eval.put` (defense-in-depth).
pub const MAX_EVAL_JSON_BYTES: usize = 4 * 1024 * 1024;

/// Narrow request kinds accepted by the experimental worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    /// `nxrMetadata.<system>` JSON.
    Metadata,
    /// `nxr.<system>` task document JSON.
    Tasks,
    /// List-shaped inventory JSON (for example flake-show excerpts).
    List,
}

impl EvalKind {
    /// Parse a protocol kind string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "metadata" => Some(Self::Metadata),
            "tasks" => Some(Self::Tasks),
            "list" => Some(Self::List),
            _ => None,
        }
    }

    /// Wire form for daemon params.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Tasks => "tasks",
            Self::List => "list",
        }
    }
}

/// Whether the CLI may attempt the experimental eval worker.
///
/// Default: **disabled**. Enabled only when `NXR_EVAL_WORKER` is `1` / `true` /
/// `on` / `yes`.
#[must_use]
pub fn eval_worker_enabled() -> bool {
    eval_worker_enabled_for(std::env::var(EVAL_WORKER_ENV).ok().as_deref())
}

#[must_use]
pub fn eval_worker_enabled_for(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "on" | "yes")
        }
        None => false,
    }
}

/// Session fingerprints that gate cache validity.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvalPrepareParams {
    /// Nix executable path + version identity (opaque client string).
    pub nix_identity: String,
    /// Optional `nix config show --json` fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_fingerprint: Option<String>,
    /// Absolute flake root path (invalidation scope).
    pub flake_root: String,
    /// Flake input fingerprint (flake.nix / lock / discovery inputs).
    pub flake_fingerprint: String,
}

#[derive(Clone, Debug)]
struct EvalEntry {
    kind: EvalKind,
    flake_root: String,
    json: Value,
}

/// In-memory eval JSON retention for one `nxrd` process.
#[derive(Debug, Default)]
pub struct EvalWorkerCache {
    nix_identity: Option<String>,
    config_fingerprint: Option<String>,
    /// flake_root → flake_fingerprint
    flake_fingerprints: BTreeMap<String, String>,
    entries: BTreeMap<String, EvalEntry>,
}

impl EvalWorkerCache {
    /// Number of retained JSON entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bind / refresh a prepare session. Returns whether any entries were dropped.
    pub fn prepare(&mut self, params: &EvalPrepareParams) -> bool {
        let mut invalidated = false;

        let identity_changed = self.nix_identity.as_deref() != Some(params.nix_identity.as_str());
        let config_changed = self.config_fingerprint != params.config_fingerprint;
        if identity_changed || config_changed {
            if !self.entries.is_empty() {
                invalidated = true;
            }
            self.entries.clear();
            self.flake_fingerprints.clear();
            self.nix_identity = Some(params.nix_identity.clone());
            self.config_fingerprint = params.config_fingerprint.clone();
        }

        let prior = self.flake_fingerprints.get(&params.flake_root).cloned();
        if prior.as_deref() != Some(params.flake_fingerprint.as_str()) {
            let removed = self.drop_root(&params.flake_root);
            if removed > 0 {
                invalidated = true;
            }
            self.flake_fingerprints
                .insert(params.flake_root.clone(), params.flake_fingerprint.clone());
        }

        if self.nix_identity.is_none() {
            self.nix_identity = Some(params.nix_identity.clone());
            self.config_fingerprint = params.config_fingerprint.clone();
        }

        invalidated
    }

    /// Look up a cached JSON value when the prepare session still matches.
    pub fn get(
        &self,
        params: &EvalPrepareParams,
        kind: EvalKind,
        cache_key: &str,
    ) -> Option<&Value> {
        if !self.session_matches(params) {
            return None;
        }
        let entry = self.entries.get(cache_key)?;
        if entry.kind != kind || entry.flake_root != params.flake_root {
            return None;
        }
        Some(&entry.json)
    }

    /// Store JSON for a matching prepare session.
    ///
    /// # Errors
    ///
    /// Returns a static message when the session mismatches or the payload is too large.
    pub fn put(
        &mut self,
        params: &EvalPrepareParams,
        kind: EvalKind,
        cache_key: &str,
        json: Value,
    ) -> Result<(), &'static str> {
        if !self.session_matches(params) {
            return Err("eval worker session mismatch; call eval.prepare first");
        }
        let encoded = serde_json::to_vec(&json).unwrap_or_default();
        if encoded.len() > MAX_EVAL_JSON_BYTES {
            return Err("eval JSON exceeds maximum retained size");
        }
        if self.entries.len() >= MAX_EVAL_ENTRIES
            && !self.entries.contains_key(cache_key)
            && let Some(first) = self.entries.keys().next().cloned()
        {
            self.entries.remove(&first);
        }
        self.entries.insert(
            cache_key.to_owned(),
            EvalEntry {
                kind,
                flake_root: params.flake_root.clone(),
                json,
            },
        );
        Ok(())
    }

    fn session_matches(&self, params: &EvalPrepareParams) -> bool {
        self.nix_identity.as_deref() == Some(params.nix_identity.as_str())
            && self.config_fingerprint == params.config_fingerprint
            && self
                .flake_fingerprints
                .get(&params.flake_root)
                .map(String::as_str)
                == Some(params.flake_fingerprint.as_str())
    }

    fn drop_root(&mut self, flake_root: &str) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| entry.flake_root != flake_root);
        before.saturating_sub(self.entries.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{EvalKind, EvalPrepareParams, EvalWorkerCache, eval_worker_enabled_for};
    use serde_json::json;

    fn params(fp: &str) -> EvalPrepareParams {
        EvalPrepareParams {
            nix_identity: "nix@1".to_owned(),
            config_fingerprint: Some("cfg".to_owned()),
            flake_root: "/repo".to_owned(),
            flake_fingerprint: fp.to_owned(),
        }
    }

    #[test]
    fn opt_in_defaults_off() {
        assert!(!eval_worker_enabled_for(None));
        assert!(!eval_worker_enabled_for(Some("0")));
        assert!(!eval_worker_enabled_for(Some("off")));
        assert!(eval_worker_enabled_for(Some("1")));
        assert!(eval_worker_enabled_for(Some("true")));
        assert!(eval_worker_enabled_for(Some("ON")));
        assert!(eval_worker_enabled_for(Some("yes")));
    }

    #[test]
    fn prepare_get_put_round_trip() {
        let mut cache = EvalWorkerCache::default();
        let p = params("fp1");
        assert!(!cache.prepare(&p));
        assert!(
            cache
                .put(&p, EvalKind::Metadata, "k", json!({"schema_version": 1}))
                .is_ok()
        );
        let got = cache.get(&p, EvalKind::Metadata, "k").expect("hit");
        assert_eq!(got["schema_version"], 1);
    }

    #[test]
    fn flake_fingerprint_change_invalidates() {
        let mut cache = EvalWorkerCache::default();
        let p1 = params("fp1");
        cache.prepare(&p1);
        cache
            .put(&p1, EvalKind::Tasks, "tasks", json!({"schema_version": 1}))
            .unwrap();
        let p2 = params("fp2");
        assert!(cache.prepare(&p2));
        assert!(cache.get(&p2, EvalKind::Tasks, "tasks").is_none());
    }

    #[test]
    fn nix_identity_change_clears_all() {
        let mut cache = EvalWorkerCache::default();
        let p1 = params("fp1");
        cache.prepare(&p1);
        cache
            .put(&p1, EvalKind::List, "list", json!({"apps": []}))
            .unwrap();
        let mut p2 = p1.clone();
        p2.nix_identity = "nix@2".to_owned();
        assert!(cache.prepare(&p2));
        assert!(cache.is_empty());
    }

    #[test]
    fn put_rejects_unprepared_session() {
        let mut cache = EvalWorkerCache::default();
        let err = cache
            .put(&params("fp1"), EvalKind::Metadata, "k", json!({}))
            .expect_err("unprepared");
        assert!(err.contains("session mismatch"));
    }

    #[test]
    fn kind_parse_round_trip() {
        assert_eq!(EvalKind::parse("metadata"), Some(EvalKind::Metadata));
        assert_eq!(EvalKind::parse("TASKS"), Some(EvalKind::Tasks));
        assert_eq!(EvalKind::parse("list"), Some(EvalKind::List));
        assert_eq!(EvalKind::parse("expr"), None);
        assert_eq!(EvalKind::Metadata.as_str(), "metadata");
    }
}
