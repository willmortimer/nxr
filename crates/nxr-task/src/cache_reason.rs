//! Context fingerprint and cache-reason helper.
//!
//! The caller passes the same identity the cache decision uses. This module does
//! not read the compiler cache and is not the MCP dispatcher.

/// Identity fields that determine context compatibility for cache decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextIdentity {
    pub rustc_version: String,
    pub wrapper_name: String,
    pub target_dir: String,
    pub cache_mode: String,
}

fn redact_field(value: &str) -> &str {
    if value.contains("://") || value.contains('@') {
        "redacted"
    } else {
        value
    }
}

/// Fingerprint material is the four identity fields (newline-joined), then digested.
///
/// Fields containing `://` or `@` are replaced with the literal `redacted` before
/// hashing. Source fingerprints, endpoints, buckets, tokens, and absolute compiler
/// paths are not included.
pub fn context_fingerprint(identity: &ContextIdentity) -> String {
    let material = [
        redact_field(&identity.rustc_version),
        redact_field(&identity.wrapper_name),
        redact_field(&identity.target_dir),
        redact_field(&identity.cache_mode),
    ]
    .join("\n");
    nxr_core::digest_bytes(material.as_bytes())
}

/// Returns exactly one cache reason for the transition from `previous` to `current`.
///
/// Priority:
/// 1. `context_unverified` when `context_verified` is false
/// 2. `toolchain_changed` when previous exists and `rustc_version` differs
/// 3. `wrapper_changed` when previous exists and `wrapper_name` differs
/// 4. `target_changed` when previous exists and `target_dir` differs
/// 5. `source_changed` when previous exists and `source_changed` is true
/// 6. `cache_cold` when previous is `None` or `cache_has_entry` is false
/// 7. `cache_hit` when previous exists, the four fields match, source is unchanged,
///    and the cache has an entry
pub fn cache_reason(
    previous: Option<&ContextIdentity>,
    current: &ContextIdentity,
    source_changed: bool,
    cache_has_entry: bool,
    context_verified: bool,
) -> &'static str {
    if !context_verified {
        return "context_unverified";
    }

    if let Some(prev) = previous {
        if prev.rustc_version != current.rustc_version {
            return "toolchain_changed";
        }
        if prev.wrapper_name != current.wrapper_name {
            return "wrapper_changed";
        }
        if prev.target_dir != current.target_dir {
            return "target_changed";
        }
        if source_changed {
            return "source_changed";
        }
        // Four-field equality is required for a hit; cache_mode has no dedicated
        // reason, so a mode-only mismatch is treated as cold.
        if prev.cache_mode != current.cache_mode || !cache_has_entry {
            return "cache_cold";
        }
        return "cache_hit";
    }

    "cache_cold"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(
        rustc_version: &str,
        wrapper_name: &str,
        target_dir: &str,
        cache_mode: &str,
    ) -> ContextIdentity {
        ContextIdentity {
            rustc_version: rustc_version.to_owned(),
            wrapper_name: wrapper_name.to_owned(),
            target_dir: target_dir.to_owned(),
            cache_mode: cache_mode.to_owned(),
        }
    }

    fn base() -> ContextIdentity {
        identity("1.75.0", "sccache", "/tmp/target", "local")
    }

    #[test]
    fn source_edit_keeps_the_context_fingerprint() {
        let before = base();
        let after = base();
        assert_eq!(context_fingerprint(&before), context_fingerprint(&after));
        assert_eq!(
            cache_reason(Some(&before), &after, true, true, true),
            "source_changed"
        );
    }

    #[test]
    fn rustc_version_changes_the_fingerprint_and_reason() {
        let previous = base();
        let current = identity("1.76.0", "sccache", "/tmp/target", "local");
        assert_ne!(
            context_fingerprint(&previous),
            context_fingerprint(&current)
        );
        assert_eq!(
            cache_reason(Some(&previous), &current, false, true, true),
            "toolchain_changed"
        );
    }

    #[test]
    fn wrapper_change_is_wrapper_changed() {
        let previous = base();
        let current = identity("1.75.0", "rustc-wrapper", "/tmp/target", "local");
        assert_ne!(
            context_fingerprint(&previous),
            context_fingerprint(&current)
        );
        assert_eq!(
            cache_reason(Some(&previous), &current, false, true, true),
            "wrapper_changed"
        );
    }

    #[test]
    fn target_change_is_target_changed() {
        let previous = base();
        let current = identity("1.75.0", "sccache", "/tmp/other-target", "local");
        assert_ne!(
            context_fingerprint(&previous),
            context_fingerprint(&current)
        );
        assert_eq!(
            cache_reason(Some(&previous), &current, false, true, true),
            "target_changed"
        );
    }

    #[test]
    fn first_use_is_cache_cold() {
        let current = base();
        assert_eq!(
            cache_reason(None, &current, false, false, true),
            "cache_cold"
        );
    }

    #[test]
    fn matching_context_with_an_entry_is_cache_hit() {
        let previous = base();
        let current = base();
        assert_eq!(
            cache_reason(Some(&previous), &current, false, true, true),
            "cache_hit"
        );
    }

    #[test]
    fn unverified_context_is_not_a_hit() {
        let previous = base();
        let current = base();
        assert_eq!(
            cache_reason(Some(&previous), &current, false, true, false),
            "context_unverified"
        );
    }

    #[test]
    fn an_endpoint_in_cache_mode_is_redacted_and_does_not_appear_in_fingerprint() {
        let with_endpoint = identity(
            "1.75.0",
            "sccache",
            "/tmp/target",
            "s3://bucket.example/cache",
        );
        let fingerprint = context_fingerprint(&with_endpoint);
        assert!(!fingerprint.contains("s3://"));
        assert!(!fingerprint.contains("bucket.example"));
        assert!(!fingerprint.contains("://"));

        let redacted_material = ["1.75.0", "sccache", "/tmp/target", "redacted"].join("\n");
        assert_eq!(
            fingerprint,
            nxr_core::digest_bytes(redacted_material.as_bytes())
        );
    }
}
