//! Execution economics JSON from caller-measured counts.
//!
//! Missing optional measurements are JSON `null` and named in `unknown`.
//! Estimates are never copied into measured savings fields.

use serde_json::{Value, json};

/// Counts already measured by the caller. `None` means not measured.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EconomicsInput {
    pub requested: u64,
    pub unique_executions: u64,
    pub coalesced: u64,
    pub reused_qualifications: u64,
    pub compiler_cache_hits: Option<u64>,
    pub wall_time_ms: Option<u64>,
    pub queue_wait_ms: Option<u64>,
    pub peak_disk_bytes: Option<u64>,
    pub artifacts_written: Option<u64>,
    pub artifacts_transferred: Option<u64>,
    pub test_compile_ms: Option<u64>,
    pub test_execute_ms: Option<u64>,
    pub measured_saved_ms: Option<u64>,
    pub estimated_saved_ms: Option<u64>,
}

const OPTIONAL_KEYS: &[&str] = &[
    "compiler_cache_hits",
    "wall_time_ms",
    "queue_wait_ms",
    "peak_disk_bytes",
    "artifacts_written",
    "artifacts_transferred",
    "test_compile_ms",
    "test_execute_ms",
    "measured_saved_ms",
    "estimated_saved_ms",
];

fn optional_value(value: Option<u64>) -> Value {
    match value {
        Some(n) => Value::from(n),
        None => Value::Null,
    }
}

/// Build the economics report object. Required counts are always numbers;
/// each optional field is a number or `null`. `unknown` lists optional keys
/// that are `null`, in schema key order.
#[must_use]
pub fn economics_report(input: &EconomicsInput) -> Value {
    let optionals = [
        input.compiler_cache_hits,
        input.wall_time_ms,
        input.queue_wait_ms,
        input.peak_disk_bytes,
        input.artifacts_written,
        input.artifacts_transferred,
        input.test_compile_ms,
        input.test_execute_ms,
        input.measured_saved_ms,
        input.estimated_saved_ms,
    ];

    let unknown: Vec<&str> = OPTIONAL_KEYS
        .iter()
        .zip(optionals)
        .filter_map(|(key, measured)| measured.is_none().then_some(*key))
        .collect();

    json!({
        "requested": input.requested,
        "unique_executions": input.unique_executions,
        "coalesced": input.coalesced,
        "reused_qualifications": input.reused_qualifications,
        "compiler_cache_hits": optional_value(input.compiler_cache_hits),
        "wall_time_ms": optional_value(input.wall_time_ms),
        "queue_wait_ms": optional_value(input.queue_wait_ms),
        "peak_disk_bytes": optional_value(input.peak_disk_bytes),
        "artifacts_written": optional_value(input.artifacts_written),
        "artifacts_transferred": optional_value(input.artifacts_transferred),
        "test_compile_ms": optional_value(input.test_compile_ms),
        "test_execute_ms": optional_value(input.test_execute_ms),
        "measured_saved_ms": optional_value(input.measured_saved_ms),
        "estimated_saved_ms": optional_value(input.estimated_saved_ms),
        "unknown": unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_optionals() -> EconomicsInput {
        EconomicsInput {
            requested: 0,
            unique_executions: 0,
            coalesced: 0,
            reused_qualifications: 0,
            compiler_cache_hits: None,
            wall_time_ms: None,
            queue_wait_ms: None,
            peak_disk_bytes: None,
            artifacts_written: None,
            artifacts_transferred: None,
            test_compile_ms: None,
            test_execute_ms: None,
            measured_saved_ms: None,
            estimated_saved_ms: None,
        }
    }

    #[test]
    fn all_optional_fields_absent_lists_unknown_keys() {
        let report = economics_report(&empty_optionals());
        for key in OPTIONAL_KEYS {
            assert!(
                report.get(key).expect("key").is_null(),
                "{key} should be null"
            );
        }
        let unknown = report["unknown"]
            .as_array()
            .expect("unknown array")
            .iter()
            .map(|v| v.as_str().expect("string"))
            .collect::<Vec<_>>();
        assert_eq!(unknown, OPTIONAL_KEYS);
    }

    #[test]
    fn measured_saved_ms_stays_separate_from_estimated() {
        let input = EconomicsInput {
            measured_saved_ms: Some(1_200),
            estimated_saved_ms: None,
            ..empty_optionals()
        };
        let report = economics_report(&input);
        assert_eq!(report["measured_saved_ms"], 1_200);
        assert!(report["estimated_saved_ms"].is_null());
        let unknown = report["unknown"].as_array().expect("unknown");
        assert!(!unknown.iter().any(|v| v == "measured_saved_ms"));
        assert!(unknown.iter().any(|v| v == "estimated_saved_ms"));
    }

    #[test]
    fn some_zero_is_reported_and_not_unknown() {
        let input = EconomicsInput {
            compiler_cache_hits: Some(0),
            wall_time_ms: Some(0),
            ..empty_optionals()
        };
        let report = economics_report(&input);
        assert_eq!(report["compiler_cache_hits"], 0);
        assert_eq!(report["wall_time_ms"], 0);
        let unknown = report["unknown"].as_array().expect("unknown");
        assert!(!unknown.iter().any(|v| v == "compiler_cache_hits"));
        assert!(!unknown.iter().any(|v| v == "wall_time_ms"));
    }

    #[test]
    fn required_counts_pass_through_unchanged() {
        let input = EconomicsInput {
            requested: 10,
            unique_executions: 12,
            coalesced: 3,
            reused_qualifications: 7,
            ..empty_optionals()
        };
        let report = economics_report(&input);
        assert_eq!(report["requested"], 10);
        assert_eq!(report["unique_executions"], 12);
        assert_eq!(report["coalesced"], 3);
        assert_eq!(report["reused_qualifications"], 7);
    }
}
