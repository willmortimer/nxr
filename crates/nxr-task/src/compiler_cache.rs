//! Compiler-cache environment for a spawned process.
//!
//! The S3 endpoint comes from the parent environment. This module does not
//! invent an endpoint, and it does not turn incremental compilation off
//! because `CI` is set.

use std::collections::BTreeMap;
use std::process::Command;

/// `sccache+remote` when an endpoint is present, `sccache` for local-only,
/// otherwise `off`. The returned label never includes the endpoint.
#[must_use]
pub fn compiler_cache_mode(env: &BTreeMap<String, String>) -> &'static str {
    match env.get("SCCACHE_ENDPOINT").map(String::as_str) {
        Some(endpoint) if !endpoint.is_empty() => "sccache+remote",
        _ if env.get("NXR_COMPILER_CACHE").map(String::as_str) == Some("sccache") => "sccache",
        _ => "off",
    }
}

/// Environment entries to add to a child. `CARGO_INCREMENTAL` is copied only
/// when the parent already set it.
#[must_use]
pub fn compiler_cache_assignments(env: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut assigned = Vec::new();
    for key in ["SCCACHE_ENDPOINT", "SCCACHE_BUCKET", "CARGO_INCREMENTAL"] {
        if let Some(value) = env.get(key).filter(|value| !value.is_empty()) {
            assigned.push((key.to_owned(), value.clone()));
        }
    }
    assigned
}

/// Apply [`compiler_cache_assignments`] unless `already` set the same key.
pub fn apply_compiler_cache(
    command: &mut Command,
    env: &BTreeMap<String, String>,
    already: &BTreeMap<String, String>,
) {
    for (key, value) in compiler_cache_assignments(env) {
        if already.contains_key(&key) {
            continue;
        }
        command.env(key, value);
    }
}

/// Parent compiler-cache variables. `CI` is not consulted.
#[must_use]
pub fn compiler_cache_from_process() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in [
        "SCCACHE_ENDPOINT",
        "SCCACHE_BUCKET",
        "CARGO_INCREMENTAL",
        "NXR_COMPILER_CACHE",
        "CI",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_owned(), value);
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_does_not_disable_incremental_and_the_endpoint_stays_out_of_the_label() {
        let mut env = BTreeMap::new();
        env.insert("CI".to_owned(), "true".to_owned());
        assert_eq!(compiler_cache_mode(&env), "off");
        assert!(
            compiler_cache_assignments(&env)
                .iter()
                .all(|(key, _)| key != "CARGO_INCREMENTAL")
        );

        env.insert(
            "SCCACHE_ENDPOINT".to_owned(),
            "https://cache.example".to_owned(),
        );
        env.insert("SCCACHE_BUCKET".to_owned(), "compiler".to_owned());
        env.insert("CARGO_INCREMENTAL".to_owned(), "1".to_owned());
        assert_eq!(compiler_cache_mode(&env), "sccache+remote");
        let assigned = compiler_cache_assignments(&env);
        assert!(
            assigned.iter().any(|(key, value)| {
                key == "SCCACHE_ENDPOINT" && value == "https://cache.example"
            })
        );
        assert!(
            assigned
                .iter()
                .any(|(key, value)| key == "CARGO_INCREMENTAL" && value == "1")
        );
        assert!(!compiler_cache_mode(&env).contains("cache.example"));
    }

    #[test]
    fn an_empty_endpoint_is_local_sccache() {
        let mut env = BTreeMap::new();
        env.insert("NXR_COMPILER_CACHE".to_owned(), "sccache".to_owned());
        env.insert("SCCACHE_ENDPOINT".to_owned(), String::new());
        assert_eq!(compiler_cache_mode(&env), "sccache");
        assert!(compiler_cache_assignments(&env).is_empty());
    }
}
