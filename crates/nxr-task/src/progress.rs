//! `nxr-progress-v1` stage snapshots.
//!
//! A missing `total` is indeterminate. This module never reads a clock and
//! never turns elapsed time into a percentage. Progress does not decide node
//! or run outcome.

use std::collections::{BTreeMap, VecDeque};

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// Schema identifier stored in every progress body.
pub const PROGRESS_V1: &str = "nxr-progress-v1";

/// Journal `event_type` for a progress snapshot.
pub const PROGRESS_EVENT_TYPE: &str = "progress";

/// Latest distinct snapshots retained per node in a projection.
pub const MAX_PROGRESS_EVENTS_PER_NODE: usize = 64;

/// Closed set of progress units. A domain-specific name uses [`ProgressUnit::Custom`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StandardUnit {
    Steps,
    Tests,
    Files,
    Bytes,
    Artifacts,
    Samples,
    Operations,
}

/// Unit for `completed` and `total`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProgressUnit {
    Standard(StandardUnit),
    Custom(CustomUnit),
}

/// Domain-specific unit name. Extra fields are rejected.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomUnit {
    pub custom: String,
}

/// Lifecycle of one stage. This is not a node or run outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Indeterminate,
}

/// One `nxr-progress-v1` snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressUpdate {
    pub schema_version: String,
    pub node_id: String,
    pub stage_id: String,
    pub stage_label: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_count"
    )]
    pub completed: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_count"
    )]
    pub total: Option<u64>,
    pub unit: ProgressUnit,
    pub state: ProgressState,
}

impl ProgressUpdate {
    /// Quantitative progress is indeterminate when `total` is absent.
    ///
    /// Elapsed time is not an input. `completed` without `total` stays
    /// indeterminate. This method does not return a percentage.
    #[must_use]
    pub const fn is_indeterminate(&self) -> bool {
        self.total.is_none()
    }

    /// Reject a document that is not `nxr-progress-v1`.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema, identity, or custom unit is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROGRESS_V1 {
            return Err(format!(
                "unsupported progress schema `{}`",
                self.schema_version
            ));
        }
        for (field, value) in [
            ("node_id", self.node_id.as_str()),
            ("stage_id", self.stage_id.as_str()),
            ("stage_label", self.stage_label.as_str()),
        ] {
            if value.is_empty() {
                return Err(format!("progress {field} is empty"));
            }
        }
        if let ProgressUnit::Custom(unit) = &self.unit
            && unit.custom.is_empty()
        {
            return Err("progress custom unit is empty".into());
        }
        Ok(())
    }
}

/// Latest observation of one stage inside a run projection.
///
/// `indeterminate` is recomputed from `total` on deserialize so a payload
/// cannot claim a ratio the counts do not support.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StageProgress {
    pub stage_id: String,
    pub stage_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub unit: ProgressUnit,
    pub state: ProgressState,
    /// True exactly when `total` is absent. Never derived from elapsed time.
    pub indeterminate: bool,
}

impl StageProgress {
    #[must_use]
    pub fn from_update(update: &ProgressUpdate) -> Self {
        Self {
            stage_id: update.stage_id.clone(),
            stage_label: update.stage_label.clone(),
            completed: update.completed,
            total: update.total,
            unit: update.unit.clone(),
            state: update.state,
            indeterminate: update.is_indeterminate(),
        }
    }

    #[must_use]
    pub const fn is_indeterminate(&self) -> bool {
        self.total.is_none()
    }
}

#[derive(Deserialize)]
struct StageProgressBody {
    stage_id: String,
    stage_label: String,
    #[serde(default)]
    completed: Option<u64>,
    #[serde(default)]
    total: Option<u64>,
    unit: ProgressUnit,
    state: ProgressState,
    /// Accepted and ignored. The flag is always `total.is_none()`.
    #[serde(default)]
    indeterminate: bool,
}

impl From<StageProgressBody> for StageProgress {
    fn from(body: StageProgressBody) -> Self {
        let _ignored = body.indeterminate;
        Self {
            stage_id: body.stage_id,
            stage_label: body.stage_label,
            completed: body.completed,
            total: body.total,
            unit: body.unit,
            state: body.state,
            indeterminate: body.total.is_none(),
        }
    }
}

impl<'de> Deserialize<'de> for StageProgress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        StageProgressBody::deserialize(deserializer).map(Self::from)
    }
}

/// Latest stage per `stage_id` for one node, after coalesce and the retention bound.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeProgressProjection {
    pub stages: BTreeMap<String, ProgressUpdate>,
    /// Set when older distinct snapshots were dropped to keep the bound.
    pub truncated: bool,
}

#[derive(Debug, Default)]
struct NodeAccum {
    recent: VecDeque<ProgressUpdate>,
    truncated: bool,
    last: Option<ProgressUpdate>,
}

/// Streaming reducer. Memory stays at the per-node snapshot bound.
#[derive(Debug, Default)]
pub struct ProgressProjector {
    nodes: BTreeMap<String, NodeAccum>,
}

impl ProgressProjector {
    pub fn push(&mut self, update: ProgressUpdate) {
        let node = self.nodes.entry(update.node_id.clone()).or_default();
        if node.last.as_ref() == Some(&update) {
            return;
        }
        node.last = Some(update.clone());
        node.recent.push_back(update);
        if node.recent.len() > MAX_PROGRESS_EVENTS_PER_NODE {
            node.recent.pop_front();
            node.truncated = true;
        }
    }

    #[must_use]
    pub fn finish(self) -> BTreeMap<String, NodeProgressProjection> {
        self.nodes
            .into_iter()
            .map(|(node_id, node)| {
                let mut stages = BTreeMap::new();
                for update in node.recent {
                    stages.insert(update.stage_id.clone(), update);
                }
                (
                    node_id,
                    NodeProgressProjection {
                        stages,
                        truncated: node.truncated,
                    },
                )
            })
            .collect()
    }
}

/// Coalesce identical consecutive snapshots, then keep the latest
/// [`MAX_PROGRESS_EVENTS_PER_NODE`] distinct snapshots per node.
#[must_use]
pub fn project_progress<I>(events: I) -> BTreeMap<String, NodeProgressProjection>
where
    I: IntoIterator<Item = ProgressUpdate>,
{
    let mut projector = ProgressProjector::default();
    for event in events {
        projector.push(event);
    }
    projector.finish()
}

fn deserialize_optional_count<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Err(de::Error::custom("progress count cannot be null")),
        serde_json::Value::Number(number) if number.is_f64() => {
            Err(de::Error::custom("progress count must be an integer"))
        }
        serde_json::Value::Number(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| de::Error::custom("progress count must be a non-negative integer")),
        _ => Err(de::Error::custom(
            "progress count must be a non-negative integer",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(
        node: &str,
        stage: &str,
        completed: Option<u64>,
        total: Option<u64>,
        state: ProgressState,
    ) -> ProgressUpdate {
        ProgressUpdate {
            schema_version: PROGRESS_V1.into(),
            node_id: node.into(),
            stage_id: stage.into(),
            stage_label: format!("label-{stage}"),
            completed,
            total,
            unit: ProgressUnit::Standard(StandardUnit::Tests),
            state,
        }
    }

    #[test]
    fn schema_example_round_trips_without_a_percentage() {
        let body = r#"{
          "schema_version": "nxr-progress-v1",
          "node_id": "rust-tests",
          "stage_id": "nextest",
          "stage_label": "NXR compiler and lint",
          "completed": 187,
          "total": 232,
          "unit": "tests",
          "state": "running"
        }"#;
        let parsed: ProgressUpdate = serde_json::from_str(body).expect("parse");
        parsed.validate().expect("valid");
        assert!(!parsed.is_indeterminate());
        assert_eq!(parsed.completed, Some(187));
        assert_eq!(parsed.total, Some(232));
        assert_eq!(parsed.unit, ProgressUnit::Standard(StandardUnit::Tests));
        assert_eq!(parsed.state, ProgressState::Running);
        let encoded = serde_json::to_value(&parsed).expect("encode");
        assert!(encoded.get("percent").is_none());
        assert!(encoded.get("elapsed_ms").is_none());
        let again: ProgressUpdate = serde_json::from_value(encoded).expect("round trip");
        assert_eq!(parsed, again);
    }

    #[test]
    fn missing_total_stays_indeterminate_and_elapsed_time_is_rejected() {
        let body = r#"{
          "schema_version": "nxr-progress-v1",
          "node_id": "rust-tests",
          "stage_id": "nextest",
          "stage_label": "NXR compiler and lint",
          "completed": 187,
          "unit": "tests",
          "state": "running"
        }"#;
        let parsed: ProgressUpdate = serde_json::from_str(body).expect("parse");
        assert!(parsed.is_indeterminate());
        assert_eq!(parsed.completed, Some(187));
        assert_eq!(parsed.total, None);
        let stage = StageProgress::from_update(&parsed);
        assert!(stage.is_indeterminate());
        assert!(stage.indeterminate);
        let lied = serde_json::json!({
            "stage_id": "nextest",
            "stage_label": "NXR compiler and lint",
            "completed": 187,
            "unit": "tests",
            "state": "running",
            "indeterminate": false
        });
        let restored: StageProgress = serde_json::from_value(lied).expect("restore");
        assert!(restored.indeterminate);
        let with_time = r#"{"schema_version":"nxr-progress-v1","node_id":"rust-tests","stage_id":"nextest","stage_label":"NXR compiler and lint","completed":1,"total":2,"unit":"tests","state":"running","elapsed_ms":50}"#;
        assert!(serde_json::from_str::<ProgressUpdate>(with_time).is_err());
        assert!(
            serde_json::from_str::<ProgressUpdate>(&body.replace(
                "\"unit\": \"tests\"",
                "\"unit\": \"tests\", \"total\": null"
            ))
            .is_err()
        );
    }

    #[test]
    fn custom_unit_and_invalid_identity_are_checked() {
        let custom: ProgressUpdate = serde_json::from_str(
            r#"{"schema_version":"nxr-progress-v1","node_id":"n","stage_id":"s","stage_label":"S","unit":{"custom":"crates"},"state":"pending"}"#,
        )
        .expect("custom");
        custom.validate().expect("valid custom");
        assert!(custom.is_indeterminate());
        let mut empty = custom.clone();
        empty.node_id.clear();
        assert!(empty.validate().is_err());
        let mut unnamed = custom.clone();
        unnamed.unit = ProgressUnit::Custom(CustomUnit {
            custom: String::new(),
        });
        assert!(unnamed.validate().is_err());
        assert!(
            serde_json::from_str::<ProgressUpdate>(
                r#"{"schema_version":"nxr-progress-v1","node_id":"n","stage_id":"s","stage_label":"S","completed":-1,"unit":"bytes","state":"running"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn replay_of_the_same_snapshots_matches_and_coalesces() {
        let mut events = vec![
            update("leaf", "compile", Some(1), Some(4), ProgressState::Running),
            update("leaf", "compile", Some(1), Some(4), ProgressState::Running),
            update("leaf", "compile", Some(1), Some(4), ProgressState::Running),
        ];
        events.push(update(
            "leaf",
            "test",
            Some(0),
            None,
            ProgressState::Running,
        ));
        events.push(update(
            "leaf",
            "compile",
            Some(4),
            Some(4),
            ProgressState::Succeeded,
        ));
        let wire = serde_json::to_string(&events).expect("wire");
        let decoded: Vec<ProgressUpdate> = serde_json::from_str(&wire).expect("decoded");
        assert_eq!(project_progress(events.clone()), project_progress(decoded));
        let projected = project_progress(events);
        assert!(!projected["leaf"].truncated);
        assert_eq!(projected["leaf"].stages.len(), 2);
        assert_eq!(projected["leaf"].stages["compile"].completed, Some(4));
        assert!(projected["leaf"].stages["test"].is_indeterminate());
        assert_eq!(
            projected["leaf"].stages["compile"].state,
            ProgressState::Succeeded
        );
    }

    #[test]
    fn identical_consecutive_snapshots_do_not_consume_the_bound() {
        let repeated = update("leaf", "compile", Some(1), Some(9), ProgressState::Running);
        let mut events = vec![repeated.clone(); MAX_PROGRESS_EVENTS_PER_NODE + 20];
        events.push(update(
            "leaf",
            "compile",
            Some(2),
            Some(9),
            ProgressState::Running,
        ));
        let projected = project_progress(events);
        assert!(!projected["leaf"].truncated);
        assert_eq!(projected["leaf"].stages["compile"].completed, Some(2));
    }

    #[test]
    fn projection_keeps_the_latest_distinct_snapshots_per_node() {
        let mut events = Vec::new();
        for index in 0..=MAX_PROGRESS_EVENTS_PER_NODE {
            events.push(update(
                "leaf",
                &format!("s{index:04}"),
                Some(u64::try_from(index).expect("index")),
                Some(80),
                ProgressState::Running,
            ));
        }
        events.push(update(
            "other",
            "solo",
            None,
            None,
            ProgressState::Indeterminate,
        ));
        let projected = project_progress(events);
        assert!(projected["leaf"].truncated);
        assert_eq!(projected["leaf"].stages.len(), MAX_PROGRESS_EVENTS_PER_NODE);
        assert!(!projected["leaf"].stages.contains_key("s0000"));
        assert!(
            projected["leaf"]
                .stages
                .contains_key(&format!("s{MAX_PROGRESS_EVENTS_PER_NODE:04}"))
        );
        assert!(!projected["other"].truncated);
        assert!(projected["other"].stages["solo"].is_indeterminate());
    }
}
