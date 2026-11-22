//! Bounded dynamic planning.
//!
//! Conditions are JSON objects. The only forms are `all`, `any`, `equals`,
//! `in`, and `compare`. `compare` accepts only `eq`, `ne`, `lt`, `lte`, `gt`,
//! and `gte`, and compares numbers. Selectors are `{"input": name}` or
//! `{"fact": name}`. Unknown forms, operators, inputs, and facts are plan
//! errors. Nothing in this module evaluates a shell, a string expression, or
//! any other language, and nothing here starts a process.
//!
//! [`plan_after_answers`] validates typed answers and returns a [`FrozenPlan`].
//! That value is not an execution permit. Domain authorization and the
//! scheduler stay outside this module.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::parameters::validate_task_parameters;
use crate::schema::TaskParameter;
use crate::typed_inputs::normalize_parameter_json;

/// Canonical document hashed into an empty discovery digest.
pub const DISCOVERY_SCHEMA: &str = "nxr-discovery-v1";

/// Version tag mixed into [`FrozenPlan::freeze_id`]. It is not a permit.
pub const FROZEN_PLAN_SCHEMA: &str = "nxr-frozen-plan-v1";

const INPUT_IDENTITY_SCHEMA: &str = "nxr-input-identity-v1";
const MAX_CONDITION_DEPTH: usize = 32;
const MAX_GROUP_CHILDREN: usize = 1024;
const MAX_QUESTIONS: usize = 128;
const MAX_BRANCHES: usize = 256;
const MAX_ANSWER_BYTES: usize = 64 * 1024;
const MAX_TOKEN_LEN: usize = 1024;

/// Failure while compiling or freezing a dynamic plan.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanningError {
    /// The object key is not `all`, `any`, `equals`, `in`, or `compare`.
    #[error("unknown condition form `{form}`")]
    UnknownForm { form: String },
    /// `compare` named an operator other than `eq`, `ne`, `lt`, `lte`, `gt`, or `gte`.
    #[error("unknown compare operator `{operator}`")]
    UnknownOperator { operator: String },
    /// An `input` selector named a value that was not resolved.
    #[error("unknown input `{name}`")]
    UnknownInput { name: String },
    /// A `fact` selector named a value this planner was not given.
    #[error("unknown fact `{name}`")]
    UnknownFact { name: String },
    /// `compare` received a value that is not a finite number.
    #[error("condition is not comparable as a number")]
    NonNumericCompare,
    /// The condition object is not one of the admitted shapes.
    #[error("invalid condition: {message}")]
    InvalidCondition { message: String },
    /// An answer did not match the question's declared type.
    #[error("invalid answer for `{name}`: {message}")]
    InvalidAnswer { name: String, message: String },
    /// A discovery question has no submitted answer.
    #[error("missing answer for `{node_id}.{name}`")]
    MissingAnswer { node_id: String, name: String },
    /// An answer does not correspond to a discovery question.
    #[error("unexpected answer `{node_id}.{name}`")]
    UnexpectedAnswer { node_id: String, name: String },
    /// Source identity, discovery, or branch metadata cannot be frozen.
    #[error("invalid plan: {message}")]
    InvalidPlan { message: String },
}

/// One dependent question returned by read-only discovery.
///
/// `name` is the flat input name used by conditions. `node_id` aligns the
/// answer with [`crate::interaction::InteractionSession::answer_values`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependentQuestion {
    pub node_id: String,
    pub name: String,
    pub declaration: TaskParameter,
}

/// Questions and the canonical digest of one admitted discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Discovery {
    pub questions: Vec<DependentQuestion>,
    pub digest: String,
}

/// Read-only discovery. The method must not start a task process.
pub trait DiscoverySource {
    /// Dependent questions and a canonical discovery digest.
    ///
    /// # Errors
    ///
    /// Returns [`PlanningError`] when the admitted observation cannot be represented.
    fn discover(&self) -> Result<Discovery, PlanningError>;
}

/// Discovery that observed nothing.
///
/// The digest is the BLAKE3 of the canonical empty discovery document and does
/// not change between calls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NullDiscovery;

impl DiscoverySource for NullDiscovery {
    fn discover(&self) -> Result<Discovery, PlanningError> {
        let body = serde_json::json!({
            "questions": [],
            "schema": DISCOVERY_SCHEMA,
        });
        Ok(Discovery {
            questions: Vec::new(),
            digest: discovery_digest(&body),
        })
    }
}

/// One conditional arm. The id is frozen only when `condition` evaluates true.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionalBranch {
    pub id: String,
    pub condition: Value,
}

/// Identity inputs for one freeze. Facts are observed values, not invented ones.
///
/// Fact values affect which branches are selected. They are not copied into the
/// frozen plan. Callers must change the discovery digest when the admitted
/// observation changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanRequest {
    pub source_revision: String,
    pub dirty: bool,
    pub declared_task_digest: String,
    pub ordinary_inputs: BTreeMap<String, Value>,
    pub facts: BTreeMap<String, Value>,
    pub branches: Vec<ConditionalBranch>,
    pub artifact_ids: BTreeSet<String>,
    pub authorization_ids: BTreeSet<String>,
}

/// Frozen source, input, discovery, and branch identity.
///
/// `freeze_id` is `freeze_` plus the BLAKE3 of the canonical JSON encoding of
/// every other field. It is not an execution permit, a lease, or a scheduler
/// admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenPlan {
    pub source_revision: String,
    pub dirty: bool,
    pub declared_task_digest: String,
    pub input_identities: BTreeMap<String, String>,
    pub discovery_digest: String,
    pub selected_branches: BTreeSet<String>,
    pub artifact_ids: BTreeSet<String>,
    pub authorization_ids: BTreeSet<String>,
    pub freeze_id: String,
}

impl FrozenPlan {
    /// Planning does not grant an execution permit.
    ///
    /// This is always [`None`]. Authorization and process launch stay with the
    /// owning system and the existing scheduler.
    #[must_use]
    pub fn execution_permit(&self) -> Option<&'static str> {
        None
    }
}

/// BLAKE3 of the canonical JSON encoding of a discovery body, prefixed for identity.
#[must_use]
pub fn discovery_digest(body: &Value) -> String {
    format!("discovery_{}", digest_canonical(body))
}

/// Evaluate one condition against resolved inputs and observed facts.
///
/// `equals` and `in` use JSON equality. `compare` orders finite numbers only.
/// `all` is true when every child is true, including when there are no children.
/// `any` is true when at least one child is true. Unknown inputs, facts, forms,
/// and operators are errors even when another child would decide the boolean.
///
/// # Errors
///
/// Returns [`PlanningError`] for an unknown form, operator, input, fact, or a
/// shape this language does not admit.
pub fn evaluate_condition(
    condition: &Value,
    inputs: &BTreeMap<String, Value>,
    facts: &BTreeMap<String, Value>,
) -> Result<bool, PlanningError> {
    let parsed = parse_condition(condition, 0)?;
    eval_condition(&parsed, inputs, facts)
}

/// Validate answers for `discovery.questions` and freeze the selected branches.
///
/// `answers` uses the same shape as
/// [`crate::interaction::InteractionSession::answer_values`]: node id, then
/// parameter name. Ordinary inputs and normalized answers share one flat input
/// namespace. The success value does not authorize disk, network, enrollment,
/// or a process.
///
/// A later call whose discovery digest or source revision differs has a
/// different [`FrozenPlan::freeze_id`].
///
/// # Errors
///
/// Returns [`PlanningError`] when questions, answers, conditions, or identity
/// fields are missing or invalid.
pub fn plan_after_answers(
    discovery: &Discovery,
    request: &PlanRequest,
    answers: &BTreeMap<String, BTreeMap<String, Value>>,
) -> Result<FrozenPlan, PlanningError> {
    validate_request(request)?;
    require_token("discovery digest", &discovery.digest)?;
    validate_questions(&request.ordinary_inputs, &discovery.questions)?;
    let resolved_answers = normalize_answers(&discovery.questions, answers)?;
    let inputs = merge_inputs(&request.ordinary_inputs, &resolved_answers)?;
    let selected_branches = select_branches(&request.branches, &inputs, &request.facts)?;
    let input_identities = input_identities(&inputs);
    let freeze_id = freeze_id(
        &request.source_revision,
        request.dirty,
        &request.declared_task_digest,
        &input_identities,
        &discovery.digest,
        &selected_branches,
        &request.artifact_ids,
        &request.authorization_ids,
    );
    Ok(FrozenPlan {
        source_revision: request.source_revision.clone(),
        dirty: request.dirty,
        declared_task_digest: request.declared_task_digest.clone(),
        input_identities,
        discovery_digest: discovery.digest.clone(),
        selected_branches,
        artifact_ids: request.artifact_ids.clone(),
        authorization_ids: request.authorization_ids.clone(),
        freeze_id,
    })
}

enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Equals(Subject, Value),
    In(Subject, Vec<Value>),
    Compare(Subject, CompareOp, Value),
}

enum Subject {
    Input(String),
    Fact(String),
}

#[derive(Clone, Copy)]
enum CompareOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

fn parse_condition(value: &Value, depth: usize) -> Result<Condition, PlanningError> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(invalid_condition("condition is nested too deeply"));
    }
    let Some(object) = value.as_object() else {
        return Err(invalid_condition("condition must be a JSON object"));
    };
    if object.len() != 1 {
        return Err(invalid_condition("condition must contain exactly one form"));
    }
    let Some((form, body)) = object.iter().next() else {
        return Err(invalid_condition("condition must contain exactly one form"));
    };
    match form.as_str() {
        "all" => parse_group(body, depth, true),
        "any" => parse_group(body, depth, false),
        "equals" => parse_equals(body),
        "in" => parse_membership(body),
        "compare" => parse_compare(body),
        other => Err(PlanningError::UnknownForm {
            form: other.to_owned(),
        }),
    }
}

fn parse_group(body: &Value, depth: usize, all: bool) -> Result<Condition, PlanningError> {
    let Some(items) = body.as_array() else {
        return Err(invalid_condition(
            "all and any require an array of conditions",
        ));
    };
    if items.len() > MAX_GROUP_CHILDREN {
        return Err(invalid_condition("condition group is too large"));
    }
    let mut children = Vec::with_capacity(items.len());
    for item in items {
        children.push(parse_condition(item, depth + 1)?);
    }
    Ok(if all {
        Condition::All(children)
    } else {
        Condition::Any(children)
    })
}

fn parse_equals(body: &Value) -> Result<Condition, PlanningError> {
    let object = object_body(body, "equals")?;
    require_only(object, &["input", "fact", "value"])?;
    let subject = parse_subject(object)?;
    let value = required_value(object, "value", "equals")?;
    Ok(Condition::Equals(subject, value))
}

fn parse_membership(body: &Value) -> Result<Condition, PlanningError> {
    let object = object_body(body, "in")?;
    require_only(object, &["input", "fact", "values"])?;
    let subject = parse_subject(object)?;
    let Some(values) = object.get("values").and_then(Value::as_array) else {
        return Err(invalid_condition("in requires a values array"));
    };
    if values.len() > MAX_GROUP_CHILDREN {
        return Err(invalid_condition("in values are too large"));
    }
    Ok(Condition::In(subject, values.clone()))
}

fn parse_compare(body: &Value) -> Result<Condition, PlanningError> {
    let object = object_body(body, "compare")?;
    require_only(object, &["input", "fact", "op", "value"])?;
    let subject = parse_subject(object)?;
    let op = match object.get("op") {
        Some(Value::String(op)) => parse_op(op)?,
        Some(_) => return Err(invalid_condition("compare op must be a string")),
        None => return Err(invalid_condition("compare requires op")),
    };
    let value = required_value(object, "value", "compare")?;
    Ok(Condition::Compare(subject, op, value))
}

fn parse_op(op: &str) -> Result<CompareOp, PlanningError> {
    match op {
        "eq" => Ok(CompareOp::Eq),
        "ne" => Ok(CompareOp::Ne),
        "lt" => Ok(CompareOp::Lt),
        "lte" => Ok(CompareOp::Lte),
        "gt" => Ok(CompareOp::Gt),
        "gte" => Ok(CompareOp::Gte),
        other => Err(PlanningError::UnknownOperator {
            operator: other.to_owned(),
        }),
    }
}

fn parse_subject(object: &Map<String, Value>) -> Result<Subject, PlanningError> {
    let input = optional_string(object, "input")?;
    let fact = optional_string(object, "fact")?;
    match (input, fact) {
        (Some(name), None) => Ok(Subject::Input(require_selector(name)?)),
        (None, Some(name)) => Ok(Subject::Fact(require_selector(name)?)),
        _ => Err(invalid_condition(
            "condition must select exactly one of input or fact",
        )),
    }
}

fn eval_condition(
    condition: &Condition,
    inputs: &BTreeMap<String, Value>,
    facts: &BTreeMap<String, Value>,
) -> Result<bool, PlanningError> {
    match condition {
        Condition::All(children) => {
            let mut matched = true;
            for child in children {
                if !eval_condition(child, inputs, facts)? {
                    matched = false;
                }
            }
            Ok(matched)
        }
        Condition::Any(children) => {
            let mut matched = false;
            for child in children {
                if eval_condition(child, inputs, facts)? {
                    matched = true;
                }
            }
            Ok(matched)
        }
        Condition::Equals(subject, expected) => Ok(lookup(subject, inputs, facts)? == expected),
        Condition::In(subject, values) => {
            let found = lookup(subject, inputs, facts)?;
            Ok(values.iter().any(|candidate| candidate == found))
        }
        Condition::Compare(subject, op, expected) => {
            let found = lookup(subject, inputs, facts)?;
            compare_numbers(*op, found, expected)
        }
    }
}

fn lookup<'a>(
    subject: &Subject,
    inputs: &'a BTreeMap<String, Value>,
    facts: &'a BTreeMap<String, Value>,
) -> Result<&'a Value, PlanningError> {
    match subject {
        Subject::Input(name) => inputs
            .get(name)
            .ok_or_else(|| PlanningError::UnknownInput { name: name.clone() }),
        Subject::Fact(name) => facts
            .get(name)
            .ok_or_else(|| PlanningError::UnknownFact { name: name.clone() }),
    }
}

fn compare_numbers(op: CompareOp, left: &Value, right: &Value) -> Result<bool, PlanningError> {
    let ordering = numeric_ordering(left, right)?;
    Ok(match op {
        CompareOp::Eq => ordering == Ordering::Equal,
        CompareOp::Ne => ordering != Ordering::Equal,
        CompareOp::Lt => ordering == Ordering::Less,
        CompareOp::Lte => ordering != Ordering::Greater,
        CompareOp::Gt => ordering == Ordering::Greater,
        CompareOp::Gte => ordering != Ordering::Less,
    })
}

fn numeric_ordering(left: &Value, right: &Value) -> Result<Ordering, PlanningError> {
    let Some(left) = left.as_number() else {
        return Err(PlanningError::NonNumericCompare);
    };
    let Some(right) = right.as_number() else {
        return Err(PlanningError::NonNumericCompare);
    };
    if let (Some(left), Some(right)) = (as_integer(left), as_integer(right)) {
        return Ok(left.cmp(&right));
    }
    let Some(left) = left.as_f64().filter(|number| number.is_finite()) else {
        return Err(PlanningError::NonNumericCompare);
    };
    let Some(right) = right.as_f64().filter(|number| number.is_finite()) else {
        return Err(PlanningError::NonNumericCompare);
    };
    left.partial_cmp(&right)
        .ok_or(PlanningError::NonNumericCompare)
}

fn as_integer(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

fn validate_request(request: &PlanRequest) -> Result<(), PlanningError> {
    require_token("source revision", &request.source_revision)?;
    require_token("declared task digest", &request.declared_task_digest)?;
    if request.branches.len() > MAX_BRANCHES {
        return Err(invalid_plan("too many conditional branches"));
    }
    let mut branch_ids = BTreeSet::new();
    for branch in &request.branches {
        require_token("branch id", &branch.id)?;
        if !branch_ids.insert(branch.id.clone()) {
            return Err(invalid_plan(format!("duplicate branch `{}`", branch.id)));
        }
    }
    for name in request.ordinary_inputs.keys() {
        require_token("input name", name)?;
    }
    for name in request.facts.keys() {
        require_token("fact name", name)?;
    }
    require_id_set("artifact id", &request.artifact_ids)?;
    require_id_set("authorization id", &request.authorization_ids)?;
    Ok(())
}

fn validate_questions(
    ordinary_inputs: &BTreeMap<String, Value>,
    questions: &[DependentQuestion],
) -> Result<(), PlanningError> {
    if questions.len() > MAX_QUESTIONS {
        return Err(invalid_plan("too many discovery questions"));
    }
    let mut names = BTreeSet::new();
    let mut by_node: BTreeMap<String, BTreeMap<String, TaskParameter>> = BTreeMap::new();
    for question in questions {
        require_token("question node", &question.node_id)?;
        if ordinary_inputs.contains_key(&question.name) {
            return Err(invalid_plan(format!(
                "discovery question `{}` duplicates a resolved input",
                question.name
            )));
        }
        if !names.insert(question.name.clone()) {
            return Err(invalid_plan(format!(
                "duplicate discovery question `{}`",
                question.name
            )));
        }
        by_node
            .entry(question.node_id.clone())
            .or_default()
            .insert(question.name.clone(), question.declaration.clone());
    }
    for (node, parameters) in &by_node {
        validate_task_parameters(node, parameters)
            .map_err(|error| invalid_plan(error.to_string()))?;
    }
    Ok(())
}

fn normalize_answers(
    questions: &[DependentQuestion],
    answers: &BTreeMap<String, BTreeMap<String, Value>>,
) -> Result<BTreeMap<String, Value>, PlanningError> {
    let expected: BTreeSet<(&str, &str)> = questions
        .iter()
        .map(|question| (question.node_id.as_str(), question.name.as_str()))
        .collect();
    for (node_id, parameters) in answers {
        if parameters.is_empty() {
            if expected.iter().any(|(node, _)| *node == node_id.as_str()) {
                continue;
            }
            return Err(invalid_plan(format!("unexpected answer node `{node_id}`")));
        }
        for (name, value) in parameters {
            if !expected.contains(&(node_id.as_str(), name.as_str())) {
                return Err(PlanningError::UnexpectedAnswer {
                    node_id: node_id.clone(),
                    name: name.clone(),
                });
            }
            let encoded =
                serde_json::to_vec(value).map_err(|_| invalid_plan("answer is not valid JSON"))?;
            if encoded.len() > MAX_ANSWER_BYTES {
                return Err(PlanningError::InvalidAnswer {
                    name: name.clone(),
                    message: "answer is too large".to_owned(),
                });
            }
        }
    }
    let mut resolved = BTreeMap::new();
    for question in questions {
        let Some(value) = answers
            .get(&question.node_id)
            .and_then(|parameters| parameters.get(&question.name))
        else {
            return Err(PlanningError::MissingAnswer {
                node_id: question.node_id.clone(),
                name: question.name.clone(),
            });
        };
        let normalized = normalize_parameter_json(
            &question.node_id,
            &question.name,
            &question.declaration,
            value,
        )
        .map_err(|error| PlanningError::InvalidAnswer {
            name: question.name.clone(),
            message: error.to_string(),
        })?;
        resolved.insert(question.name.clone(), normalized);
    }
    Ok(resolved)
}

fn merge_inputs(
    ordinary: &BTreeMap<String, Value>,
    answers: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, PlanningError> {
    let mut merged = ordinary.clone();
    for (name, value) in answers {
        if merged.insert(name.clone(), value.clone()).is_some() {
            return Err(invalid_plan(format!(
                "discovery question `{name}` duplicates a resolved input"
            )));
        }
    }
    Ok(merged)
}

fn select_branches(
    branches: &[ConditionalBranch],
    inputs: &BTreeMap<String, Value>,
    facts: &BTreeMap<String, Value>,
) -> Result<BTreeSet<String>, PlanningError> {
    let mut selected = BTreeSet::new();
    for branch in branches {
        let condition = parse_condition(&branch.condition, 0)?;
        if eval_condition(&condition, inputs, facts)? {
            selected.insert(branch.id.clone());
        }
    }
    Ok(selected)
}

fn input_identities(inputs: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    inputs
        .iter()
        .map(|(name, value)| (name.clone(), input_identity(name, value)))
        .collect()
}

fn input_identity(name: &str, value: &Value) -> String {
    let body = serde_json::json!({
        "name": name,
        "schema": INPUT_IDENTITY_SCHEMA,
        "value": value,
    });
    format!("input_{}", digest_canonical(&body))
}

fn freeze_id(
    source_revision: &str,
    dirty: bool,
    declared_task_digest: &str,
    input_identities: &BTreeMap<String, String>,
    discovery_digest: &str,
    selected_branches: &BTreeSet<String>,
    artifact_ids: &BTreeSet<String>,
    authorization_ids: &BTreeSet<String>,
) -> String {
    let document = serde_json::json!({
        "artifact_ids": id_list(artifact_ids),
        "authorization_ids": id_list(authorization_ids),
        "declared_task_digest": declared_task_digest,
        "dirty": dirty,
        "discovery_digest": discovery_digest,
        "input_identities": input_identities,
        "schema": FROZEN_PLAN_SCHEMA,
        "selected_branches": id_list(selected_branches),
        "source_revision": source_revision,
    });
    format!("freeze_{}", digest_canonical(&document))
}

fn digest_canonical(value: &Value) -> String {
    let canonical = crate::identity::canonical_json(value);
    nxr_core::digest_bytes(canonical.as_bytes())
}

fn id_list(ids: &BTreeSet<String>) -> Vec<&str> {
    ids.iter().map(String::as_str).collect()
}

fn object_body<'a>(body: &'a Value, form: &str) -> Result<&'a Map<String, Value>, PlanningError> {
    body.as_object()
        .ok_or_else(|| invalid_condition(format!("{form} must be an object")))
}

fn required_value(
    object: &Map<String, Value>,
    key: &str,
    form: &str,
) -> Result<Value, PlanningError> {
    object
        .get(key)
        .cloned()
        .ok_or_else(|| invalid_condition(format!("{form} requires {key}")))
}

fn optional_string(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, PlanningError> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(name)) => Ok(Some(name.clone())),
        Some(_) => Err(invalid_condition(format!("`{key}` must be a string"))),
    }
}

fn require_selector(name: String) -> Result<String, PlanningError> {
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(invalid_condition("input and fact names must be non-empty"));
    }
    Ok(name)
}

fn require_only(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), PlanningError> {
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(PlanningError::UnknownForm {
            form: unknown.clone(),
        });
    }
    Ok(())
}

fn require_id_set(label: &str, ids: &BTreeSet<String>) -> Result<(), PlanningError> {
    for id in ids {
        require_token(label, id)?;
    }
    Ok(())
}

fn require_token(label: &str, value: &str) -> Result<(), PlanningError> {
    if value.is_empty() || value.len() > MAX_TOKEN_LEN || value.chars().any(char::is_control) {
        return Err(invalid_plan(format!("invalid {label}")));
    }
    Ok(())
}

fn invalid_condition(message: impl Into<String>) -> PlanningError {
    PlanningError::InvalidCondition {
        message: message.into(),
    }
}

fn invalid_plan(message: impl Into<String>) -> PlanningError {
    PlanningError::InvalidPlan {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn ids(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn string_parameter() -> TaskParameter {
        TaskParameter {
            param_type: crate::schema::TaskParameterType::String,
            default: None,
            values: None,
            constraints: None,
        }
    }

    fn boolean_parameter() -> TaskParameter {
        TaskParameter {
            param_type: crate::schema::TaskParameterType::Boolean,
            default: None,
            values: None,
            constraints: None,
        }
    }

    fn wifi_request() -> PlanRequest {
        PlanRequest {
            source_revision: "rev-1".to_owned(),
            dirty: false,
            declared_task_digest: "task-digest-1".to_owned(),
            ordinary_inputs: map(&[
                ("network_mode", json!("wifi")),
                ("label", json!("sentinel-label")),
            ]),
            facts: map(&[("wifi_supported", json!(true))]),
            branches: vec![
                ConditionalBranch {
                    id: "wireless".to_owned(),
                    condition: json!({
                        "all": [
                            {"equals": {"input": "network_mode", "value": "wifi"}},
                            {"equals": {"fact": "wifi_supported", "value": true}}
                        ]
                    }),
                },
                ConditionalBranch {
                    id: "wired".to_owned(),
                    condition: json!({"equals": {"input": "network_mode", "value": "ethernet"}}),
                },
            ],
            artifact_ids: ids(&["artifact-1"]),
            authorization_ids: ids(&["auth-1"]),
        }
    }

    fn freeze(discovery: &Discovery, request: &PlanRequest) -> Result<FrozenPlan, PlanningError> {
        plan_after_answers(discovery, request, &BTreeMap::new())
    }

    #[test]
    fn evaluates_equality_membership_compare_all_and_any() {
        let inputs = map(&[("network_mode", json!("wifi")), ("level", json!(5))]);
        let facts = map(&[("wifi_supported", json!(true)), ("bands", json!("five"))]);

        assert!(
            evaluate_condition(
                &json!({"equals": {"input": "network_mode", "value": "wifi"}}),
                &inputs,
                &facts,
            )
            .expect("equals")
        );
        assert!(
            !evaluate_condition(
                &json!({"equals": {"input": "network_mode", "value": true}}),
                &inputs,
                &facts,
            )
            .expect("equals does not coerce")
        );
        assert!(
            evaluate_condition(
                &json!({"equals": {"fact": "wifi_supported", "value": true}}),
                &inputs,
                &facts,
            )
            .expect("fact equals")
        );
        assert!(
            evaluate_condition(
                &json!({
                    "all": [
                        {"equals": {"input": "network_mode", "value": "wifi"}},
                        {"equals": {"fact": "wifi_supported", "value": true}}
                    ]
                }),
                &inputs,
                &facts,
            )
            .expect("documented all")
        );
        assert!(
            !evaluate_condition(
                &json!({"all": [{"equals": {"input": "network_mode", "value": "ethernet"}}]}),
                &inputs,
                &facts,
            )
            .expect("all is false")
        );
        assert!(evaluate_condition(&json!({"all": []}), &inputs, &facts).expect("empty all"));
        assert!(
            evaluate_condition(
                &json!({
                    "any": [
                        {"equals": {"input": "network_mode", "value": "ethernet"}},
                        {"equals": {"fact": "wifi_supported", "value": true}}
                    ]
                }),
                &inputs,
                &facts,
            )
            .expect("any")
        );
        assert!(
            !evaluate_condition(
                &json!({"any": [{"equals": {"input": "network_mode", "value": "ethernet"}}]}),
                &inputs,
                &facts,
            )
            .expect("any is false")
        );
        assert!(!evaluate_condition(&json!({"any": []}), &inputs, &facts).expect("empty any"));
        assert!(
            evaluate_condition(
                &json!({"in": {"input": "network_mode", "values": ["ethernet", "wifi"]}}),
                &inputs,
                &facts,
            )
            .expect("membership")
        );
        assert!(
            !evaluate_condition(
                &json!({"in": {"fact": "bands", "values": ["two", "six"]}}),
                &inputs,
                &facts,
            )
            .expect("membership miss")
        );
        assert!(
            !evaluate_condition(
                &json!({"in": {"input": "network_mode", "values": ["wi"]}}),
                &inputs,
                &facts,
            )
            .expect("membership is not a substring search")
        );

        let comparisons = [
            ("eq", json!(5), true),
            ("ne", json!(5), false),
            ("lt", json!(5), false),
            ("lt", json!(6), true),
            ("lte", json!(5), true),
            ("lte", json!(4), false),
            ("gt", json!(4), true),
            ("gt", json!(5), false),
            ("gte", json!(5), true),
            ("gte", json!(6), false),
        ];
        for (op, value, expected) in comparisons {
            let actual = evaluate_condition(
                &json!({"compare": {"input": "level", "op": op, "value": value}}),
                &inputs,
                &facts,
            )
            .expect("compare");
            assert_eq!(actual, expected, "{op} {value}");
        }
        assert_eq!(
            evaluate_condition(
                &json!({"compare": {"fact": "wifi_supported", "op": "eq", "value": 1}}),
                &inputs,
                &facts,
            ),
            Err(PlanningError::NonNumericCompare)
        );
        assert!(
            evaluate_condition(
                &json!({"compare": {"input": "level", "op": "lt", "value": 0}}),
                &map(&[("level", json!(-1))]),
                &BTreeMap::new(),
            )
            .expect("negative compare")
        );
    }

    #[test]
    fn refuses_unknown_forms_operators_inputs_and_facts() {
        let inputs = map(&[("network_mode", json!("wifi"))]);
        let empty = BTreeMap::new();
        assert_eq!(
            evaluate_condition(&json!({"shell": "true"}), &empty, &empty),
            Err(PlanningError::UnknownForm {
                form: "shell".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(
                &json!({"equals": {"input": "network_mode", "value": "wifi", "shell": "true"}}),
                &inputs,
                &empty,
            ),
            Err(PlanningError::UnknownForm {
                form: "shell".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(&json!("network_mode == 'wifi'"), &inputs, &empty),
            Err(PlanningError::InvalidCondition {
                message: "condition must be a JSON object".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(
                &json!({"compare": {"input": "network_mode", "op": "like", "value": 1}}),
                &inputs,
                &empty,
            ),
            Err(PlanningError::UnknownOperator {
                operator: "like".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(
                &json!({"equals": {"input": "missing", "value": "wifi"}}),
                &inputs,
                &empty,
            ),
            Err(PlanningError::UnknownInput {
                name: "missing".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(
                &json!({"equals": {"fact": "wifi_supported", "value": true}}),
                &inputs,
                &empty,
            ),
            Err(PlanningError::UnknownFact {
                name: "wifi_supported".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(
                &json!({
                    "any": [
                        {"equals": {"input": "network_mode", "value": "wifi"}},
                        {"equals": {"input": "missing", "value": true}}
                    ]
                }),
                &inputs,
                &empty,
            ),
            Err(PlanningError::UnknownInput {
                name: "missing".to_owned(),
            })
        );
        assert_eq!(
            evaluate_condition(
                &json!({"compare": {"input": "network_mode", "op": "eq", "value": "wifi"}}),
                &inputs,
                &empty,
            ),
            Err(PlanningError::NonNumericCompare)
        );
        assert_eq!(
            evaluate_condition(
                &json!({"equals": {"input": "network_mode", "fact": "other", "value": "wifi"}}),
                &inputs,
                &empty,
            ),
            Err(PlanningError::InvalidCondition {
                message: "condition must select exactly one of input or fact".to_owned(),
            })
        );
    }

    #[test]
    fn null_discovery_digest_is_stable() {
        let first = NullDiscovery.discover().expect("null discovery");
        let second = NullDiscovery.discover().expect("null discovery");
        assert!(first.questions.is_empty());
        assert_eq!(first, second);
        assert_eq!(
            first.digest,
            discovery_digest(&json!({"questions": [], "schema": DISCOVERY_SCHEMA}))
        );
        assert!(first.digest.starts_with("discovery_"));
        let hex = first.digest.trim_start_matches("discovery_");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }

    #[test]
    fn changed_discovery_or_source_invalidates_freeze_id() {
        let discovery = NullDiscovery.discover().expect("null discovery");
        let request = wifi_request();
        let plan = freeze(&discovery, &request).expect("plan");
        let encoded = serde_json::to_string(&plan).expect("serialize identities");
        assert!(!encoded.contains("wifi"));
        assert!(!encoded.contains("sentinel-label"));
        let repeat = freeze(&discovery, &request).expect("repeat");
        assert_eq!(plan.freeze_id, repeat.freeze_id);
        assert!(plan.selected_branches.contains("wireless"));
        assert!(!plan.selected_branches.contains("wired"));
        assert!(plan.freeze_id.starts_with("freeze_"));

        let mut other_discovery = discovery.clone();
        other_discovery.digest = discovery_digest(&json!({"observation": "other"}));
        let rediscovered = freeze(&other_discovery, &request).expect("other discovery");
        assert_ne!(plan.freeze_id, rediscovered.freeze_id);

        let mut revised = request.clone();
        revised.source_revision = "rev-2".to_owned();
        let revised_plan = freeze(&discovery, &revised).expect("other revision");
        assert_ne!(plan.freeze_id, revised_plan.freeze_id);

        let mut dirty = request.clone();
        dirty.dirty = true;
        assert_ne!(
            plan.freeze_id,
            freeze(&discovery, &dirty).expect("dirty").freeze_id
        );

        let mut task = request.clone();
        task.declared_task_digest = "task-digest-2".to_owned();
        assert_ne!(
            plan.freeze_id,
            freeze(&discovery, &task).expect("task").freeze_id
        );

        let mut labeled = request.clone();
        labeled
            .ordinary_inputs
            .insert("label".to_owned(), json!("other-label"));
        assert_ne!(
            plan.freeze_id,
            freeze(&discovery, &labeled).expect("input").freeze_id
        );
        assert_eq!(
            freeze(&discovery, &labeled)
                .expect("input")
                .selected_branches,
            plan.selected_branches
        );

        let mut unsupported = request.clone();
        unsupported
            .facts
            .insert("wifi_supported".to_owned(), json!(false));
        let unselected = freeze(&discovery, &unsupported).expect("facts");
        assert!(!unselected.selected_branches.contains("wireless"));
        assert_ne!(plan.freeze_id, unselected.freeze_id);

        let mut artifacts = request.clone();
        artifacts.artifact_ids = ids(&["artifact-2"]);
        assert_ne!(
            plan.freeze_id,
            freeze(&discovery, &artifacts).expect("artifact").freeze_id
        );

        let mut authorizations = request.clone();
        authorizations.authorization_ids = ids(&["auth-2"]);
        assert_ne!(
            plan.freeze_id,
            freeze(&discovery, &authorizations)
                .expect("authorization")
                .freeze_id
        );

        let mut reordered = request.clone();
        reordered.branches.reverse();
        assert_eq!(
            plan.freeze_id,
            freeze(&discovery, &reordered).expect("order").freeze_id
        );
    }

    #[test]
    fn plan_after_answers_validates_answers_and_does_not_permit_execution() {
        let discovery = Discovery {
            questions: vec![DependentQuestion {
                node_id: "link".to_owned(),
                name: "network_mode".to_owned(),
                declaration: string_parameter(),
            }],
            digest: discovery_digest(&json!({"observation": "link"})),
        };
        let request = PlanRequest {
            source_revision: "rev-1".to_owned(),
            dirty: false,
            declared_task_digest: "task-digest-1".to_owned(),
            ordinary_inputs: BTreeMap::new(),
            facts: map(&[("wifi_supported", json!(true))]),
            branches: vec![ConditionalBranch {
                id: "wireless".to_owned(),
                condition: json!({
                    "all": [
                        {"equals": {"input": "network_mode", "value": "wifi"}},
                        {"equals": {"fact": "wifi_supported", "value": true}}
                    ]
                }),
            }],
            artifact_ids: ids(&["artifact-1"]),
            authorization_ids: ids(&["auth-1"]),
        };
        let mut answers = BTreeMap::new();
        answers.insert("link".to_owned(), map(&[("network_mode", json!("wifi"))]));
        let plan = plan_after_answers(&discovery, &request, &answers).expect("frozen");
        assert!(plan.selected_branches.contains("wireless"));
        assert!(plan.input_identities.contains_key("network_mode"));
        assert_eq!(plan.execution_permit(), None);
        let encoded = serde_json::to_string(&plan).expect("serialize identities");
        assert!(!encoded.contains("wifi"));
        assert!(!encoded.contains("ExecutionLease"));
        assert!(!encoded.contains("supervisor"));
        let _frozen: FrozenPlan = plan;

        assert!(matches!(
            plan_after_answers(&discovery, &request, &BTreeMap::new()),
            Err(PlanningError::MissingAnswer { .. })
        ));
        let mut wrong_type = BTreeMap::new();
        wrong_type.insert("link".to_owned(), map(&[("network_mode", json!(true))]));
        assert!(matches!(
            plan_after_answers(&discovery, &request, &wrong_type),
            Err(PlanningError::InvalidAnswer { .. })
        ));

        let mut boolean_question = discovery.clone();
        boolean_question.questions[0].declaration = boolean_parameter();
        boolean_question.digest = discovery_digest(&json!({"observation": "boolean"}));
        assert!(matches!(
            plan_after_answers(&boolean_question, &request, &answers),
            Err(PlanningError::InvalidAnswer { .. })
        ));

        let mut shell_branch = request.clone();
        shell_branch.branches[0].condition = json!({"shell": "true"});
        assert_eq!(
            plan_after_answers(&discovery, &shell_branch, &answers),
            Err(PlanningError::UnknownForm {
                form: "shell".to_owned(),
            })
        );
        let mut bad_op = request.clone();
        bad_op.branches[0].condition =
            json!({"compare": {"input": "network_mode", "op": "like", "value": 1}});
        assert_eq!(
            plan_after_answers(&discovery, &bad_op, &answers),
            Err(PlanningError::UnknownOperator {
                operator: "like".to_owned(),
            })
        );
    }
}
