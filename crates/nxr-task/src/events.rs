//! Typed execution events and a synchronous sink trait.
//!
//! Renderers and schedulers share this bus so scheduling stays decoupled from
//! presentation. Events are sync-only (no Tokio).
//!
//! Chunk payloads are byte-safe: pipes emit raw bytes, JSONL may label them as
//! UTF-8 or base64 (`encoding`), and human renderers decode UTF-8 incrementally.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Wire encoding label for stdout/stderr chunk payloads (JSONL).
///
/// Absent `encoding` on the wire means UTF-8 (backward compatible).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkEncoding {
    /// `text` is a UTF-8 string.
    #[default]
    Utf8,
    /// `text` is standard base64 of arbitrary bytes.
    Base64,
}

impl ChunkEncoding {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8",
            Self::Base64 => "base64",
        }
    }
}

/// Byte-safe stdout/stderr payload carried by chunk events.
///
/// Serializes as `text` plus optional `encoding` (`utf8` omitted for
/// compatibility; `base64` always written).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputPayload {
    /// Valid UTF-8 text.
    Utf8(String),
    /// Arbitrary bytes (invalid UTF-8 or intentionally opaque).
    Bytes(Vec<u8>),
}

impl OutputPayload {
    /// Construct a UTF-8 payload.
    #[must_use]
    pub fn utf8(text: impl Into<String>) -> Self {
        Self::Utf8(text.into())
    }

    /// Prefer [`Self::Utf8`] when `bytes` is valid UTF-8; otherwise [`Self::Bytes`].
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self::Utf8(text),
            Err(err) => Self::Bytes(err.into_bytes()),
        }
    }

    /// Borrow the underlying bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Utf8(text) => text.as_bytes(),
            Self::Bytes(bytes) => bytes.as_slice(),
        }
    }

    /// Wire encoding used when serializing this payload.
    #[must_use]
    pub const fn encoding(&self) -> ChunkEncoding {
        match self {
            Self::Utf8(_) => ChunkEncoding::Utf8,
            Self::Bytes(_) => ChunkEncoding::Base64,
        }
    }
}

impl Serialize for OutputPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Utf8(text) => {
                // Omit encoding for UTF-8 so existing fixtures stay stable.
                let mut state = serializer.serialize_struct("OutputPayload", 1)?;
                state.serialize_field("text", text)?;
                state.end()
            }
            Self::Bytes(bytes) => {
                let mut state = serializer.serialize_struct("OutputPayload", 2)?;
                state.serialize_field("text", &BASE64.encode(bytes))?;
                state.serialize_field("encoding", &ChunkEncoding::Base64)?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for OutputPayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(OutputPayloadVisitor)
    }
}

struct OutputPayloadVisitor;

impl<'de> Visitor<'de> for OutputPayloadVisitor {
    type Value = OutputPayload;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("chunk payload with text and optional encoding")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut text: Option<String> = None;
        let mut encoding: Option<ChunkEncoding> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "text" => {
                    if text.is_some() {
                        return Err(de::Error::duplicate_field("text"));
                    }
                    text = Some(map.next_value()?);
                }
                "encoding" => {
                    if encoding.is_some() {
                        return Err(de::Error::duplicate_field("encoding"));
                    }
                    encoding = Some(map.next_value()?);
                }
                other => {
                    return Err(de::Error::unknown_field(other, &["text", "encoding"]));
                }
            }
        }

        let text = text.ok_or_else(|| de::Error::missing_field("text"))?;
        match encoding.unwrap_or(ChunkEncoding::Utf8) {
            ChunkEncoding::Utf8 => Ok(OutputPayload::Utf8(text)),
            ChunkEncoding::Base64 => {
                let bytes = BASE64
                    .decode(text.as_bytes())
                    .map_err(|err| de::Error::custom(format!("invalid base64 chunk: {err}")))?;
                Ok(OutputPayload::Bytes(bytes))
            }
        }
    }
}

/// Typed events emitted during plan construction and node execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An immutable execution plan was produced.
    PlanCreated {
        /// Root task id the plan was built for.
        root: String,
        /// Number of nodes in the plan.
        node_count: usize,
    },
    /// A node entered the ready/queued set.
    NodeQueued {
        /// Task id.
        node: String,
    },
    /// A node process (or equivalent) started.
    NodeStarted {
        /// Task id.
        node: String,
    },
    /// A chunk of stdout from a running node.
    StdoutChunk {
        /// Task id.
        node: String,
        /// Byte-safe payload (`text` + optional `encoding` on the wire).
        #[serde(flatten)]
        payload: OutputPayload,
    },
    /// A chunk of stderr from a running node.
    StderrChunk {
        /// Task id.
        node: String,
        /// Byte-safe payload (`text` + optional `encoding` on the wire).
        #[serde(flatten)]
        payload: OutputPayload,
    },
    /// A node finished with an exit status.
    NodeExited {
        /// Task id.
        node: String,
        /// Process exit code (`None` when terminated by signal / unavailable).
        code: Option<i32>,
    },
    /// The overall run finished.
    RunCompleted {
        /// Whether every required node succeeded under the active failure policy.
        success: bool,
    },
    /// Non-fatal diagnostic for operators / renderers.
    Diagnostic {
        /// Human-readable message (already sanitized for terminals by the emitter).
        message: String,
    },
}

/// Synchronous consumer of [`Event`] values.
pub trait EventSink {
    /// Receive one event.
    fn emit(&mut self, event: Event);
}

/// Sink that records every event in order (useful in tests and dry-runs).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordingSink {
    events: Vec<Event>,
}

impl RecordingSink {
    /// Create an empty recording sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow recorded events in emission order.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Take ownership of recorded events, leaving the sink empty.
    #[must_use]
    pub fn into_events(self) -> Vec<Event> {
        self.events
    }

    /// Clear recorded events.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
}

/// No-op sink for callers that discard events.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&mut self, _event: Event) {}
}

/// Exhaustively classify an event for diagnostics / tests.
///
/// Returning a static label forces new variants to be handled at compile time.
#[must_use]
pub fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::PlanCreated { .. } => "plan_created",
        Event::NodeQueued { .. } => "node_queued",
        Event::NodeStarted { .. } => "node_started",
        Event::StdoutChunk { .. } => "stdout_chunk",
        Event::StderrChunk { .. } => "stderr_chunk",
        Event::NodeExited { .. } => "node_exited",
        Event::RunCompleted { .. } => "run_completed",
        Event::Diagnostic { .. } => "diagnostic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};
    use std::collections::BTreeSet;

    /// Checked-in JSON Schema for the `Event` wire shape (Phase 16 / X1).
    const EVENTS_SCHEMA: &str = include_str!("../../../schemas/events-v1.schema.json");
    /// Fixture samples covering every `Event` variant (including null exit code).
    const EVENTS_SAMPLES: &str = include_str!("../../../tests/fixtures/events-v1-samples.json");

    /// Stable labels that must remain aligned with [`event_kind`] and the schema.
    const ALL_EVENT_KINDS: &[&str] = &[
        "plan_created",
        "node_queued",
        "node_started",
        "stdout_chunk",
        "stderr_chunk",
        "node_exited",
        "run_completed",
        "diagnostic",
    ];

    fn sample_events() -> Vec<Event> {
        vec![
            Event::PlanCreated {
                root: "d".to_owned(),
                node_count: 4,
            },
            Event::NodeQueued {
                node: "a".to_owned(),
            },
            Event::NodeStarted {
                node: "a".to_owned(),
            },
            Event::StdoutChunk {
                node: "a".to_owned(),
                payload: OutputPayload::utf8("hello"),
            },
            Event::StderrChunk {
                node: "a".to_owned(),
                payload: OutputPayload::utf8("warn"),
            },
            Event::NodeExited {
                node: "a".to_owned(),
                code: Some(1),
            },
            Event::NodeExited {
                node: "b".to_owned(),
                code: None,
            },
            Event::RunCompleted { success: false },
            Event::Diagnostic {
                message: "cycle avoided".to_owned(),
            },
        ]
    }

    /// Structural check against `events-v1.schema.json` without a JSON Schema crate.
    fn assert_matches_events_schema(value: &Value) {
        let schema: Value = serde_json::from_str(EVENTS_SCHEMA).expect("parse events schema");
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .expect("$defs");
        let obj = value.as_object().expect("event object");
        let type_name = obj
            .get("type")
            .and_then(Value::as_str)
            .expect("type string");
        let def = defs
            .get(type_name)
            .unwrap_or_else(|| panic!("schema missing $defs.{type_name}"));
        let required = def
            .get("required")
            .and_then(Value::as_array)
            .expect("required");
        for key in required {
            let key = key.as_str().expect("required key");
            assert!(
                obj.contains_key(key),
                "event type={type_name} missing required field `{key}`: {value}"
            );
        }
        if def.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
            let allowed: BTreeSet<&str> = def
                .get("properties")
                .and_then(Value::as_object)
                .expect("properties")
                .keys()
                .map(String::as_str)
                .collect();
            for key in obj.keys() {
                assert!(
                    allowed.contains(key.as_str()),
                    "event type={type_name} has unexpected field `{key}`: {value}"
                );
            }
        }
        // Spot-check property types declared in the schema.
        let props = def
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties");
        for (key, prop_schema) in props {
            if key == "type" {
                let expected = prop_schema
                    .get("const")
                    .and_then(Value::as_str)
                    .expect("type const");
                assert_eq!(type_name, expected);
                continue;
            }
            let Some(field) = obj.get(key) else {
                continue;
            };
            assert_json_type(field, prop_schema, type_name, key);
        }
    }

    fn assert_json_type(value: &Value, prop_schema: &Value, event_type: &str, field: &str) {
        match prop_schema.get("type") {
            Some(Value::String(ty)) => match ty.as_str() {
                "string" => {
                    assert!(
                        value.is_string(),
                        "{event_type}.{field} expected string: {value}"
                    );
                    if let Some(Value::Array(allowed)) = prop_schema.get("enum") {
                        let s = value.as_str().expect("string");
                        let ok = allowed.iter().any(|v| v.as_str() == Some(s));
                        assert!(
                            ok,
                            "{event_type}.{field} value `{s}` not in enum {allowed:?}"
                        );
                    }
                }
                "integer" => assert!(
                    value.as_i64().is_some() || value.as_u64().is_some(),
                    "{event_type}.{field} expected integer: {value}"
                ),
                "boolean" => assert!(
                    value.is_boolean(),
                    "{event_type}.{field} expected boolean: {value}"
                ),
                other => panic!("unsupported schema type `{other}` for {event_type}.{field}"),
            },
            Some(Value::Array(types)) => {
                let ok = types.iter().any(|ty| match ty.as_str() {
                    Some("string") => value.is_string(),
                    Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
                    Some("boolean") => value.is_boolean(),
                    Some("null") => value.is_null(),
                    _ => false,
                });
                assert!(
                    ok,
                    "{event_type}.{field} does not match any of {types:?}: {value}"
                );
            }
            other => panic!("missing type in schema for {event_type}.{field}: {other:?}"),
        }
    }

    fn schema_event_kinds() -> BTreeSet<String> {
        let schema: Value = serde_json::from_str(EVENTS_SCHEMA).expect("parse events schema");
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .expect("$defs");
        let one_of = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .expect("oneOf");
        let mut kinds = BTreeSet::new();
        for entry in one_of {
            let reference = entry.get("$ref").and_then(Value::as_str).expect("$ref");
            let name = reference.strip_prefix("#/$defs/").expect("$defs ref");
            assert!(
                defs.contains_key(name),
                "oneOf references missing $defs.{name}"
            );
            let const_type = defs[name]
                .pointer("/properties/type/const")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("$defs.{name}.properties.type.const"));
            assert_eq!(const_type, name, "def name must match type const");
            kinds.insert(name.to_owned());
        }
        kinds
    }

    #[test]
    fn recording_sink_preserves_order() {
        let mut sink = RecordingSink::new();
        sink.emit(Event::PlanCreated {
            root: "ci".to_owned(),
            node_count: 2,
        });
        sink.emit(Event::NodeQueued {
            node: "fmt".to_owned(),
        });
        sink.emit(Event::NodeStarted {
            node: "fmt".to_owned(),
        });
        sink.emit(Event::StdoutChunk {
            node: "fmt".to_owned(),
            payload: OutputPayload::utf8("ok\n"),
        });
        sink.emit(Event::StderrChunk {
            node: "fmt".to_owned(),
            payload: OutputPayload::utf8(""),
        });
        sink.emit(Event::NodeExited {
            node: "fmt".to_owned(),
            code: Some(0),
        });
        sink.emit(Event::RunCompleted { success: true });
        sink.emit(Event::Diagnostic {
            message: "done".to_owned(),
        });

        assert_eq!(sink.events().len(), 8);
        assert_eq!(event_kind(&sink.events()[0]), "plan_created");
        assert_eq!(event_kind(&sink.events()[7]), "diagnostic");
    }

    #[test]
    fn event_json_round_trip() {
        for event in sample_events() {
            let encoded = serde_json::to_value(&event).expect("serialize");
            let decoded: Event = serde_json::from_value(encoded).expect("deserialize");
            assert_eq!(decoded, event);
            // Touch every variant via the exhaustive classifier.
            assert!(!event_kind(&decoded).is_empty());
        }
    }

    #[test]
    fn binary_chunk_round_trips_as_base64() {
        let bytes = vec![0x00, 0xff, 0xfe, 0x80, b'A'];
        let event = Event::StdoutChunk {
            node: "bin".to_owned(),
            payload: OutputPayload::from_bytes(bytes.clone()),
        };
        assert!(matches!(event, Event::StdoutChunk { payload: OutputPayload::Bytes(_), .. }));

        let encoded = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            encoded.get("encoding").and_then(Value::as_str),
            Some("base64")
        );
        assert_matches_events_schema(&encoded);

        let decoded: Event = serde_json::from_value(encoded).expect("deserialize");
        match decoded {
            Event::StdoutChunk { payload, .. } => {
                assert_eq!(payload.as_bytes(), bytes.as_slice());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn utf8_chunk_omits_encoding_on_wire() {
        let event = Event::StdoutChunk {
            node: "a".to_owned(),
            payload: OutputPayload::utf8("café"),
        };
        let encoded = serde_json::to_value(&event).expect("serialize");
        assert!(encoded.get("encoding").is_none());
        assert_eq!(encoded.get("text").and_then(Value::as_str), Some("café"));
        assert_matches_events_schema(&encoded);
    }

    #[test]
    fn events_schema_covers_all_event_kinds() {
        let schema_kinds = schema_event_kinds();
        let expected: BTreeSet<String> = ALL_EVENT_KINDS.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(schema_kinds, expected);

        // Exhaustive classifier stays aligned with the published schema labels.
        for event in sample_events() {
            assert!(
                expected.contains(event_kind(&event)),
                "event_kind {} missing from schema set",
                event_kind(&event)
            );
        }
    }

    #[test]
    fn serialized_events_match_events_v1_schema() {
        for event in sample_events() {
            let encoded = serde_json::to_value(&event).expect("serialize");
            assert_eq!(
                encoded.get("type").and_then(Value::as_str),
                Some(event_kind(&event))
            );
            assert_matches_events_schema(&encoded);
        }
    }

    #[test]
    fn fixture_events_round_trip_and_match_schema() {
        let values: Vec<Value> = serde_json::from_str(EVENTS_SAMPLES).expect("parse fixture");
        assert_eq!(values.len(), 10);

        let mut seen = BTreeSet::new();
        for value in values {
            assert_matches_events_schema(&value);
            let decoded: Event =
                serde_json::from_value(value.clone()).expect("deserialize fixture");
            seen.insert(event_kind(&decoded).to_owned());
            let reencoded = serde_json::to_value(&decoded).expect("re-serialize");
            assert_eq!(reencoded, value);
            assert_matches_events_schema(&reencoded);
        }

        let expected: BTreeSet<String> = ALL_EVENT_KINDS.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn events_schema_document_shape() {
        let schema: Map<String, Value> =
            serde_json::from_str(EVENTS_SCHEMA).expect("parse events schema");
        assert_eq!(
            schema.get("$id").and_then(Value::as_str),
            Some("https://nxr.dev/schemas/events-v1.schema.json")
        );
        assert_eq!(
            schema.get("$schema").and_then(Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );
        assert!(schema.get("oneOf").and_then(Value::as_array).is_some());
        assert!(schema.get("$defs").and_then(Value::as_object).is_some());
    }

    #[test]
    fn null_sink_discards() {
        let mut sink = NullSink;
        sink.emit(Event::Diagnostic {
            message: "ignored".to_owned(),
        });
    }
}
