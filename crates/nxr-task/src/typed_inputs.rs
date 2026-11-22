//! Typed, node-local input values. References describe evidence; they never verify it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::parameters::{ParameterError, parameter_env_name};
use crate::schema::{ParameterPathKind, TaskParameter, TaskParameterType};

pub const INPUT_VALUES_SCHEMA: &str = "nxr-input-values-v1";

/// A values document, not a workflow or authorization document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputValuesDocument {
    pub schema_version: String,
    pub inputs: BTreeMap<String, BTreeMap<String, Value>>,
}

/// Decode an input document, rejecting unsupported versions and unknown fields.
pub fn parse_input_values(value: &Value) -> Result<InputValuesDocument, ParameterError> {
    let document: InputValuesDocument = serde_json::from_value(value.clone())
        .map_err(|_| invalid("inputs", "document", "invalid input-values document"))?;
    if document.schema_version != INPUT_VALUES_SCHEMA {
        return Err(invalid(
            "inputs",
            "schema_version",
            "unsupported input-values schema",
        ));
    }
    if document.inputs.keys().any(|node| node.trim().is_empty()) {
        return Err(invalid(
            "inputs",
            "node",
            "node selectors must not be empty",
        ));
    }
    Ok(document)
}

/// Origin metadata is safe to persist independently of the resolved values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputProvenance {
    ScopedCli,
    Cli,
    InputFile,
    Context,
    Environment,
    Default,
    Answer,
}

/// Deliberately not serializable: callers must choose public metadata explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInput {
    pub value: Value,
    pub provenance: InputProvenance,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedInputs {
    pub values: BTreeMap<String, ResolvedInput>,
    pub secret_references: BTreeSet<String>,
}

impl ResolvedInputs {
    /// Canonical child delivery. Secret values are never resolved here.
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, String> {
        self.values
            .iter()
            .map(|(name, input)| {
                (
                    parameter_env_name(name),
                    input_environment_value(&input.value),
                )
            })
            .collect()
    }

    #[must_use]
    pub fn provenance(&self) -> BTreeMap<String, InputProvenance> {
        self.values
            .iter()
            .map(|(name, input)| (name.clone(), input.provenance))
            .collect()
    }

    #[must_use]
    pub fn has_secret_references(&self) -> bool {
        !self.secret_references.is_empty()
    }
}

/// Each map is already restricted to this node and keyed by parameter name.
/// CLI ambiguity and unknown-node validation belong to the plan resolver.
#[derive(Clone, Copy, Default)]
pub struct InputLayers<'a> {
    pub scoped_cli: Option<&'a BTreeMap<String, Value>>,
    pub cli: Option<&'a BTreeMap<String, Value>>,
    pub file: Option<&'a BTreeMap<String, Value>>,
    pub context: Option<&'a BTreeMap<String, Value>>,
    pub environment: Option<&'a BTreeMap<String, Value>>,
    pub answers: Option<&'a BTreeMap<String, Value>>,
    /// Absolute caller invocation directory for declared local-path checks.
    pub base_directory: Option<&'a Path>,
}

pub fn resolve_node_inputs(
    node: &str,
    parameters: &BTreeMap<String, TaskParameter>,
    layers: InputLayers<'_>,
) -> Result<ResolvedInputs, ParameterError> {
    crate::parameters::validate_task_parameters(node, parameters)
        .map_err(|_| invalid(node, "declaration", "invalid typed input declaration"))?;
    let ordered = [
        (layers.scoped_cli, InputProvenance::ScopedCli),
        (layers.cli, InputProvenance::Cli),
        (layers.file, InputProvenance::InputFile),
        (layers.context, InputProvenance::Context),
        (layers.environment, InputProvenance::Environment),
    ];
    for (map, _) in ordered
        .iter()
        .copied()
        .chain(std::iter::once((layers.answers, InputProvenance::Answer)))
    {
        if let Some(map) = map
            && let Some(name) = map.keys().find(|name| !parameters.contains_key(*name))
        {
            return Err(ParameterError::UnknownSet {
                name: format!("{node}.{name}"),
            });
        }
    }
    let mut resolved = ResolvedInputs::default();
    for (name, definition) in parameters {
        let selected = ordered
            .iter()
            .find_map(|(map, origin)| {
                map.and_then(|map| map.get(name))
                    .map(|value| (value, *origin))
            })
            .or_else(|| {
                definition
                    .default
                    .as_ref()
                    .map(|value| (value, InputProvenance::Default))
            })
            .or_else(|| {
                layers
                    .answers
                    .and_then(|map| map.get(name))
                    .map(|value| (value, InputProvenance::Answer))
            })
            .ok_or_else(|| ParameterError::Missing {
                task: node.to_owned(),
                name: name.clone(),
                env_name: parameter_env_name(name),
            })?;
        let normalized = normalize_parameter_json(node, name, definition, selected.0)?;
        validate_local_paths(node, name, definition, &normalized, layers.base_directory)?;
        if parameter_contains_secret_reference(definition) {
            resolved.secret_references.insert(name.clone());
        }
        resolved.values.insert(
            name.clone(),
            ResolvedInput {
                value: normalized,
                provenance: selected.1,
            },
        );
    }
    Ok(resolved)
}

/// True for direct or nested secret references; cache policy must honor this.
#[must_use]
pub fn parameter_contains_secret_reference(parameter: &TaskParameter) -> bool {
    parameter.param_type == TaskParameterType::SecretRef
        || parameter.constraints.as_ref().is_some_and(|constraints| {
            constraints
                .properties
                .values()
                .any(parameter_contains_secret_reference)
        })
}

#[must_use]
pub fn input_environment_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => canonical_json(value).to_string(),
    }
}

/// Convert string CLI/environment input to its declared JSON type.
pub fn parse_parameter_text(
    task: &str,
    name: &str,
    parameter: &TaskParameter,
    raw: &str,
) -> Result<Value, ParameterError> {
    match parameter.param_type {
        TaskParameterType::String | TaskParameterType::Choice | TaskParameterType::Path => {
            Ok(Value::String(raw.to_owned()))
        }
        TaskParameterType::Duration => Ok(raw.parse::<u64>().map_or_else(
            |_| Value::String(raw.to_owned()),
            |millis| serde_json::json!(millis),
        )),
        TaskParameterType::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err(invalid(
                task,
                name,
                "boolean parameter must be true or false",
            )),
        },
        _ => serde_json::from_str(raw).map_err(|_| {
            invalid(
                task,
                name,
                "expected a JSON value of the declared input type",
            )
        }),
    }
}

/// Pure normalization and envelope validation. No reference is marked trusted.
pub fn normalize_parameter_json(
    task: &str,
    name: &str,
    parameter: &TaskParameter,
    value: &Value,
) -> Result<Value, ParameterError> {
    let constraints = parameter.constraints.as_ref();
    let normalized = match parameter.param_type {
        TaskParameterType::String | TaskParameterType::Choice | TaskParameterType::Path => {
            let string = value
                .as_str()
                .ok_or_else(|| invalid(task, name, "expected a string"))?;
            if string.contains('\0') {
                return Err(invalid(task, name, "input contains a NUL character"));
            }
            if parameter.param_type == TaskParameterType::Path && string.is_empty() {
                return Err(invalid(task, name, "path must not be empty"));
            }
            if parameter.param_type == TaskParameterType::Choice
                && !parameter
                    .values
                    .as_ref()
                    .is_some_and(|values| values.iter().any(|allowed| allowed == string))
            {
                return Err(invalid(task, name, "value is outside the declared choices"));
            }
            if let Some(constraints) = constraints {
                let length = string.chars().count();
                if constraints
                    .min_length
                    .is_some_and(|minimum| length < minimum)
                    || constraints
                        .max_length
                        .is_some_and(|maximum| length > maximum)
                {
                    return Err(invalid(
                        task,
                        name,
                        "string length violates the declared bounds",
                    ));
                }
                if let Some(pattern) = &constraints.pattern {
                    let expression = regex::Regex::new(pattern)
                        .map_err(|_| invalid(task, name, "invalid string pattern"))?;
                    if !expression.is_match(string) {
                        return Err(invalid(
                            task,
                            name,
                            "string does not match the declared pattern",
                        ));
                    }
                }
                if parameter.param_type == TaskParameterType::Path
                    && constraints.path.as_ref().is_some_and(|path| path.absolute)
                    && !Path::new(string).is_absolute()
                {
                    return Err(invalid(task, name, "path must be absolute"));
                }
            }
            Value::String(string.to_owned())
        }
        TaskParameterType::Boolean => Value::Bool(
            value
                .as_bool()
                .ok_or_else(|| invalid(task, name, "expected a boolean"))?,
        ),
        TaskParameterType::Integer => {
            let integer =
                integer_value(value).ok_or_else(|| invalid(task, name, "expected an integer"))?;
            if let Some(constraints) = constraints {
                let minimum = constraints.minimum.as_ref().and_then(number_integer);
                let maximum = constraints.maximum.as_ref().and_then(number_integer);
                if minimum.is_some_and(|minimum| integer < minimum)
                    || maximum.is_some_and(|maximum| integer > maximum)
                {
                    return Err(invalid(task, name, "integer violates the declared bounds"));
                }
                if let Some(step) = constraints.step.as_ref().and_then(number_integer)
                    && (step <= 0 || (integer - minimum.unwrap_or(0)) % step != 0)
                {
                    return Err(invalid(task, name, "integer violates the declared step"));
                }
            }
            value.clone()
        }
        TaskParameterType::Number => {
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| invalid(task, name, "expected a finite number"))?;
            if let Some(constraints) = constraints {
                let minimum = constraints
                    .minimum
                    .as_ref()
                    .and_then(serde_json::Number::as_f64);
                let maximum = constraints
                    .maximum
                    .as_ref()
                    .and_then(serde_json::Number::as_f64);
                if minimum.is_some_and(|minimum| number < minimum)
                    || maximum.is_some_and(|maximum| number > maximum)
                {
                    return Err(invalid(task, name, "number violates the declared bounds"));
                }
                if let Some(step) = constraints
                    .step
                    .as_ref()
                    .and_then(serde_json::Number::as_f64)
                {
                    let steps = (number - minimum.unwrap_or(0.0)) / step;
                    if step <= 0.0
                        || !steps.is_finite()
                        || (steps - steps.round()).abs()
                            > f64::EPSILON * 16.0 * steps.abs().max(1.0)
                    {
                        return Err(invalid(task, name, "number violates the declared step"));
                    }
                }
            }
            // One representation for numerically equal integral JSON numbers.
            if number == 0.0 {
                serde_json::json!(0)
            } else if let Some(integer) = value.as_i64() {
                serde_json::json!(integer)
            } else if let Some(integer) = value.as_u64() {
                serde_json::json!(integer)
            } else if number.fract() == 0.0 && number >= i64::MIN as f64 && number < i64::MAX as f64
            {
                serde_json::json!(number as i64)
            } else {
                serde_json::json!(number)
            }
        }
        TaskParameterType::Multichoice => {
            let array = value
                .as_array()
                .ok_or_else(|| invalid(task, name, "expected an array of choices"))?;
            let allowed = parameter
                .values
                .as_ref()
                .ok_or_else(|| invalid(task, name, "multichoice requires allowed values"))?;
            let mut choices = BTreeSet::new();
            for choice in array {
                let choice = choice
                    .as_str()
                    .ok_or_else(|| invalid(task, name, "multichoice entries must be strings"))?;
                if !allowed.iter().any(|allowed| allowed == choice)
                    || !choices.insert(choice.to_owned())
                {
                    return Err(invalid(
                        task,
                        name,
                        "multichoice contains an unknown or duplicate value",
                    ));
                }
            }
            if constraints.is_some_and(|constraints| {
                constraints
                    .min_items
                    .is_some_and(|minimum| choices.len() < minimum)
                    || constraints
                        .max_items
                        .is_some_and(|maximum| choices.len() > maximum)
            }) {
                return Err(invalid(
                    task,
                    name,
                    "selection count violates declared bounds",
                ));
            }
            Value::Array(choices.into_iter().map(Value::String).collect())
        }
        TaskParameterType::Duration => {
            let millis = if let Some(millis) = value.as_u64() {
                millis
            } else {
                parse_duration_millis(value.as_str().ok_or_else(|| {
                    invalid(task, name, "expected duration text or integer milliseconds")
                })?)
                .ok_or_else(|| {
                    invalid(
                        task,
                        name,
                        "duration must fit unsigned milliseconds (ms/s/m/h)",
                    )
                })?
            };
            serde_json::json!(millis)
        }
        TaskParameterType::Object => {
            let object = value
                .as_object()
                .ok_or_else(|| invalid(task, name, "expected a strict object"))?;
            let empty = BTreeMap::new();
            let properties = constraints.map_or(&empty, |constraints| &constraints.properties);
            if object.keys().any(|key| !properties.contains_key(key)) {
                return Err(invalid(
                    task,
                    name,
                    "object contains an undeclared property",
                ));
            }
            let mut normalized = serde_json::Map::new();
            for (property, declaration) in properties {
                if let Some(value) = object.get(property).or(declaration.default.as_ref()) {
                    normalized.insert(
                        property.clone(),
                        normalize_parameter_json(
                            task,
                            &format!("{name}.{property}"),
                            declaration,
                            value,
                        )?,
                    );
                } else if constraints
                    .is_some_and(|constraints| constraints.required.contains(property))
                {
                    return Err(invalid(task, name, "object is missing a required property"));
                }
            }
            Value::Object(normalized)
        }
        TaskParameterType::ArtifactRef => {
            let reference: ArtifactInputReference = serde_json::from_value(value.clone())
                .map_err(|_| invalid(task, name, "invalid artifact reference envelope"))?;
            if !valid_digest(&reference.digest)
                || reference.producer.trim().is_empty()
                || reference.reference.trim().is_empty()
            {
                return Err(invalid(
                    task,
                    name,
                    "artifact reference requires digest, producer and locator",
                ));
            }
            value.clone()
        }
        TaskParameterType::SecretRef => {
            let reference: SecretInputReference =
                serde_json::from_value(value.clone()).map_err(|_| {
                    invalid(
                        task,
                        name,
                        "invalid secret reference envelope; plaintext is not accepted",
                    )
                })?;
            if reference.reference.trim().is_empty() || reference.reference.contains('\0') {
                return Err(invalid(task, name, "secret reference must not be empty"));
            }
            value.clone()
        }
        TaskParameterType::ObservedFact | TaskParameterType::AuthorizationRef => {
            let reference: AuthorityInputReference = serde_json::from_value(value.clone())
                .map_err(|_| invalid(task, name, "invalid authority reference envelope"))?;
            if reference.authority.trim().is_empty()
                || reference.reference.trim().is_empty()
                || !valid_digest(&reference.digest)
            {
                return Err(invalid(
                    task,
                    name,
                    "authority reference requires authority, reference and digest",
                ));
            }
            value.clone()
        }
    };
    Ok(canonical_json(&normalized))
}

fn contains_security_reference(parameter: &TaskParameter) -> bool {
    parameter.param_type.is_security_reference()
        || parameter.constraints.as_ref().is_some_and(|constraints| {
            constraints
                .properties
                .values()
                .any(contains_security_reference)
        })
}

/// Validate declaration constraints before any values are accepted.
pub fn validate_parameter_constraints(
    task: &str,
    name: &str,
    parameter: &TaskParameter,
) -> Result<(), ParameterError> {
    if contains_security_reference(parameter) && parameter.default.is_some() {
        return Err(invalid(
            task,
            name,
            "security references cannot have implicit defaults",
        ));
    }
    if parameter.param_type == TaskParameterType::Multichoice {
        let Some(values) = parameter
            .values
            .as_ref()
            .filter(|values| !values.is_empty())
        else {
            return Err(invalid(
                task,
                name,
                "multichoice requires nonempty allowed values",
            ));
        };
        if values.iter().collect::<BTreeSet<_>>().len() != values.len() {
            return Err(invalid(
                task,
                name,
                "multichoice allowed values must be unique",
            ));
        }
    }
    let Some(constraints) = parameter.constraints.as_ref() else {
        return Ok(());
    };
    let string = matches!(
        parameter.param_type,
        TaskParameterType::String | TaskParameterType::Choice | TaskParameterType::Path
    );
    let numeric = matches!(
        parameter.param_type,
        TaskParameterType::Integer | TaskParameterType::Number
    );
    if !string
        && (constraints.min_length.is_some()
            || constraints.max_length.is_some()
            || constraints.pattern.is_some())
    {
        return Err(invalid(
            task,
            name,
            "string constraints require a string, choice, or path",
        ));
    }
    if !numeric
        && (constraints.minimum.is_some()
            || constraints.maximum.is_some()
            || constraints.step.is_some())
    {
        return Err(invalid(
            task,
            name,
            "numeric constraints require an integer or number",
        ));
    }
    if parameter.param_type != TaskParameterType::Multichoice
        && (constraints.min_items.is_some() || constraints.max_items.is_some())
    {
        return Err(invalid(task, name, "selection bounds require multichoice"));
    }
    if parameter.param_type != TaskParameterType::Path && constraints.path.is_some() {
        return Err(invalid(task, name, "path constraints require a path"));
    }
    if parameter.param_type != TaskParameterType::Object
        && (!constraints.properties.is_empty() || !constraints.required.is_empty())
    {
        return Err(invalid(task, name, "object constraints require an object"));
    }
    if constraints
        .min_length
        .zip(constraints.max_length)
        .is_some_and(|(min, max)| min > max)
        || constraints
            .min_items
            .zip(constraints.max_items)
            .is_some_and(|(min, max)| min > max)
    {
        return Err(invalid(task, name, "minimum exceeds maximum"));
    }
    if let Some(pattern) = &constraints.pattern {
        regex::Regex::new(pattern).map_err(|_| invalid(task, name, "invalid string pattern"))?;
    }
    if parameter.param_type == TaskParameterType::Integer {
        if constraints
            .minimum
            .iter()
            .chain(constraints.maximum.iter())
            .chain(constraints.step.iter())
            .any(|number| number_integer(number).is_none())
        {
            return Err(invalid(
                task,
                name,
                "integer bounds and step must be integers",
            ));
        }
        if constraints
            .minimum
            .as_ref()
            .and_then(number_integer)
            .zip(constraints.maximum.as_ref().and_then(number_integer))
            .is_some_and(|(min, max)| min > max)
        {
            return Err(invalid(task, name, "minimum exceeds maximum"));
        }
    } else if constraints
        .minimum
        .as_ref()
        .and_then(serde_json::Number::as_f64)
        .zip(
            constraints
                .maximum
                .as_ref()
                .and_then(serde_json::Number::as_f64),
        )
        .is_some_and(|(min, max)| min > max)
    {
        return Err(invalid(task, name, "minimum exceeds maximum"));
    }
    if constraints
        .step
        .as_ref()
        .and_then(serde_json::Number::as_f64)
        .is_some_and(|step| !step.is_finite() || step <= 0.0)
    {
        return Err(invalid(task, name, "step must be positive and finite"));
    }
    if constraints
        .required
        .iter()
        .any(|required| !constraints.properties.contains_key(required))
        || constraints.required.iter().collect::<BTreeSet<_>>().len() != constraints.required.len()
    {
        return Err(invalid(
            task,
            name,
            "required must name unique declared object properties",
        ));
    }
    if !constraints.properties.is_empty() {
        crate::parameters::validate_task_parameters(task, &constraints.properties)
            .map_err(|_| invalid(task, name, "invalid nested object declaration"))?;
    }
    if let Some(default) = &parameter.default {
        normalize_parameter_json(task, name, parameter, default)?;
    }
    Ok(())
}

/// Resolve declared local existence/type constraints separately from pure normalization.
pub fn validate_local_paths(
    task: &str,
    name: &str,
    declaration: &TaskParameter,
    value: &Value,
    base: Option<&Path>,
) -> Result<(), ParameterError> {
    if declaration.param_type == TaskParameterType::Object
        && let (Some(constraints), Some(object)) = (&declaration.constraints, value.as_object())
    {
        for (property, nested) in &constraints.properties {
            if let Some(value) = object.get(property) {
                validate_local_paths(task, &format!("{name}.{property}"), nested, value, base)?;
            }
        }
    }
    if declaration.param_type != TaskParameterType::Path {
        return Ok(());
    }
    let Some(constraints) = declaration
        .constraints
        .as_ref()
        .and_then(|constraints| constraints.path.as_ref())
    else {
        return Ok(());
    };
    if !constraints.exists && constraints.kind == ParameterPathKind::Any {
        return Ok(());
    }
    let value = value
        .as_str()
        .ok_or_else(|| invalid(task, name, "expected a path string"))?;
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        base.filter(|base| base.is_absolute())
            .ok_or_else(|| {
                invalid(
                    task,
                    name,
                    "local path checks require an absolute invocation directory",
                )
            })?
            .join(path)
    };
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !constraints.exists => {
            return Ok(());
        }
        Err(_) => {
            return Err(invalid(
                task,
                name,
                "path does not satisfy local existence/access constraints",
            ));
        }
    };
    if (constraints.kind == ParameterPathKind::File && !metadata.is_file())
        || (constraints.kind == ParameterPathKind::Directory && !metadata.is_dir())
    {
        return Err(invalid(
            task,
            name,
            "path has the wrong local filesystem kind",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactInputReference {
    digest: String,
    producer: String,
    reference: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretInputReference {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    #[allow(dead_code)]
    provider: crate::schema::SecretProvider,
    #[serde(default)]
    #[allow(dead_code)]
    delivery: Option<crate::schema::SecretDelivery>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityInputReference {
    authority: String,
    reference: String,
    digest: String,
}

fn integer_value(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}
fn number_integer(value: &serde_json::Number) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}
fn valid_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
fn invalid(task: &str, name: &str, message: &str) -> ParameterError {
    ParameterError::Invalid {
        task: task.to_owned(),
        name: name.to_owned(),
        message: message.to_owned(),
    }
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(array) => Value::Array(array.iter().map(canonical_json).collect()),
        _ => value.clone(),
    }
}

fn parse_duration_millis(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split].parse().ok()?;
    let multiplier = match value[split..].trim() {
        "ms" => 1,
        "" | "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration(value: Value) -> TaskParameter {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn precedence_and_node_local_defaults_do_not_collide() {
        let declaration = declaration(json!({"type":"integer","default":2}));
        let parameters = BTreeMap::from([("count".to_owned(), declaration)]);
        let cli = BTreeMap::from([("count".to_owned(), json!(8))]);
        let file = BTreeMap::from([("count".to_owned(), json!(4))]);
        let first = resolve_node_inputs(
            "first",
            &parameters,
            InputLayers {
                scoped_cli: Some(&cli),
                file: Some(&file),
                ..Default::default()
            },
        )
        .unwrap();
        let second = resolve_node_inputs("second", &parameters, InputLayers::default()).unwrap();
        assert_eq!(first.environment()["NXR_PARAM_COUNT"], "8");
        assert_eq!(second.environment()["NXR_PARAM_COUNT"], "2");
        assert_eq!(first.provenance()["count"], InputProvenance::ScopedCli);
    }

    #[test]
    fn canonical_multichoice_and_duration_have_equal_identity_inputs() {
        let choices = declaration(json!({"type":"multichoice","values":["a","b"]}));
        assert_eq!(
            normalize_parameter_json("task", "choices", &choices, &json!(["b", "a"])).unwrap(),
            json!(["a", "b"])
        );
        let duration = declaration(json!({"type":"duration"}));
        assert_eq!(
            normalize_parameter_json("task", "time", &duration, &json!("2s")).unwrap(),
            json!(2000)
        );
        assert!(
            normalize_parameter_json("task", "time", &duration, &json!("18446744073709551615h"))
                .is_err()
        );
    }

    #[test]
    fn integer_range_and_step_are_exact_above_float_precision() {
        let number = declaration(
            json!({"type":"integer","constraints":{"minimum":9007199254740992_u64,"maximum":9007199254740994_u64,"step":2}}),
        );
        assert!(
            normalize_parameter_json("task", "n", &number, &json!(9007199254740993_u64)).is_err()
        );
        assert!(
            normalize_parameter_json("task", "n", &number, &json!(9007199254740994_u64)).is_ok()
        );
        assert!(normalize_parameter_json("task", "n", &number, &json!(1.5)).is_err());
    }

    #[test]
    fn strict_nested_objects_and_reference_envelopes_refuse_fabricated_trust() {
        let object = declaration(
            json!({"type":"object","constraints":{"properties":{"name":{"type":"string","constraints":{"pattern":"^[a-z]+$"}}},"required":["name"]}}),
        );
        assert!(
            normalize_parameter_json("task", "o", &object, &json!({"name":"ok","extra":true}))
                .is_err()
        );
        assert!(normalize_parameter_json("task", "o", &object, &json!({"name":"123"})).is_err());
        let fact = declaration(json!({"type":"observed_fact"}));
        assert!(normalize_parameter_json("task", "disk", &fact, &json!({"authority":"nxi","reference":"obs","digest":format!("sha256:{}", "a".repeat(64)),"verified":true})).is_err());
        let secret = declaration(json!({"type":"secret_ref"}));
        let sentinel = "PLAINTEXT_SENTINEL";
        let error =
            normalize_parameter_json("task", "secret", &secret, &json!(sentinel)).unwrap_err();
        assert!(!error.to_string().contains(sentinel));
    }

    #[test]
    fn nested_security_references_cannot_be_supplied_by_object_defaults() {
        for kind in ["secret_ref", "observed_fact", "authorization_ref"] {
            let reference = if kind == "secret_ref" {
                json!({"ref":"fixture://credential"})
            } else {
                json!({"authority":"nxi","reference":"fixture-reference","digest":format!("sha256:{}", "a".repeat(64))})
            };
            let nested =
                json!({"type":"object","constraints":{"properties":{"credential":{"type":kind}}}});
            let outer_default = declaration(json!({
                "type":"object", "default":{"nested":{"credential":reference.clone()}},
                "constraints":{"properties":{"nested":nested.clone()}}
            }));
            assert!(
                validate_parameter_constraints("task", "settings", &outer_default).is_err(),
                "outer default must not provide {kind}"
            );
            let mut nested_default = nested;
            nested_default["default"] = json!({"credential":reference});
            let inner_default = declaration(
                json!({"type":"object","constraints":{"properties":{"nested":nested_default}}}),
            );
            assert!(
                validate_parameter_constraints("task", "settings", &inner_default).is_err(),
                "nested default must not provide {kind}"
            );
        }
        let public_default = declaration(
            json!({"type":"object","default":{"count":3},"constraints":{"properties":{"count":{"type":"integer"}}}}),
        );
        assert!(validate_parameter_constraints("task", "settings", &public_default).is_ok());
    }

    #[test]
    fn input_document_rejects_unknown_fields_and_versions() {
        assert!(
            parse_input_values(
                &json!({"schema_version":"nxr-input-values-v1","inputs":{},"trusted":true})
            )
            .is_err()
        );
        assert!(
            parse_input_values(&json!({"schema_version":"nxr-input-values-v2","inputs":{}}))
                .is_err()
        );
    }

    #[test]
    fn declared_local_path_checks_are_not_silently_skipped() {
        let path = declaration(
            json!({"type":"path","constraints":{"path":{"exists":true,"kind":"file"}}}),
        );
        let root = tempfile::tempdir().unwrap();
        let parameters = BTreeMap::from([("path".to_owned(), path)]);
        let file = BTreeMap::from([("path".to_owned(), json!("missing"))]);
        assert!(
            resolve_node_inputs(
                "task",
                &parameters,
                InputLayers {
                    file: Some(&file),
                    base_directory: Some(root.path()),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}
