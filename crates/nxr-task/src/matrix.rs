//! Matrix `include` expansion for task planning and spawn-time env injection.
//!
//! Matrix attribute **values** are injected as `NXR_MATRIX_<KEY>` environment
//! variables when a matrix-expanded child is spawned. Plans and events carry
//! matrix attribute names only.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::parameters::is_canonical_ident;
use crate::schema::{SchemaError, TaskDefinition, TaskMatrix};

/// Prefix for spawn-time matrix attribute environment variables.
pub const MATRIX_ENV_PREFIX: &str = "NXR_MATRIX_";

/// One expanded matrix instance for a base task id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatrixInstance {
    /// Canonical task id before expansion.
    pub base_task: String,
    /// Matrix include attributes for this instance (values never logged).
    pub attrs: BTreeMap<String, JsonValue>,
}

/// Result of expanding matrix tasks for planning and execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatrixExpansion {
    /// Runnable task map (matrix templates replaced by per-include instances).
    pub tasks: BTreeMap<String, TaskDefinition>,
    /// Expanded node id → matrix instance metadata.
    pub instances: BTreeMap<String, MatrixInstance>,
    /// Base task id → expanded instance ids (include order).
    pub instance_ids: BTreeMap<String, Vec<String>>,
}

/// Build the spawn-time environment variable name for a matrix attribute key.
#[must_use]
pub fn matrix_env_name(key: &str) -> String {
    format!("{}{}", MATRIX_ENV_PREFIX, key.to_ascii_uppercase())
}

/// Sorted matrix attribute names for plan metadata (values never included).
#[must_use]
pub fn matrix_attr_names(attrs: &BTreeMap<String, JsonValue>) -> Vec<String> {
    let mut names: Vec<String> = attrs.keys().cloned().collect();
    names.sort();
    names
}

/// Normalized matrix attribute values keyed by include attribute name.
#[must_use]
pub fn resolve_matrix_values(attrs: &BTreeMap<String, JsonValue>) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for (key, value) in attrs {
        if let Some(normalized) = matrix_value_to_string(value) {
            values.insert(key.clone(), normalized);
        }
    }
    values
}

/// Resolve spawn-time `NXR_MATRIX_*` assignments for a matrix instance.
#[must_use]
pub fn resolve_matrix_env(attrs: &BTreeMap<String, JsonValue>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, value) in resolve_matrix_values(attrs) {
        env.insert(matrix_env_name(&key), value);
    }
    env
}

/// Expand matrix `include` entries into runnable task nodes and dependency wiring.
///
/// # Errors
///
/// Returns [`SchemaError::InvalidMatrix`] when matrix metadata is invalid.
pub fn expand_matrix_tasks(
    tasks: &BTreeMap<String, TaskDefinition>,
) -> Result<MatrixExpansion, SchemaError> {
    let mut instance_ids = BTreeMap::new();
    for (name, definition) in tasks {
        if let Some(matrix) = &definition.matrix {
            validate_task_matrix(name, matrix)?;
            let mut ids = Vec::with_capacity(matrix.include.len());
            for (index, attrs) in matrix.include.iter().enumerate() {
                validate_matrix_attrs(name, index, attrs)?;
                ids.push(matrix_node_id(name, index));
            }
            instance_ids.insert(name.clone(), ids);
        }
    }

    let mut expanded = BTreeMap::new();
    let mut instances = BTreeMap::new();
    for (name, definition) in tasks {
        if let Some(matrix) = &definition.matrix {
            for (index, attrs) in matrix.include.iter().enumerate() {
                let id = matrix_node_id(name, index);
                let mut instance_def = definition.clone();
                instance_def.matrix = None;
                instance_def.depends_on = expand_depends_on(&definition.depends_on, &instance_ids);
                expanded.insert(id.clone(), instance_def);
                instances.insert(
                    id,
                    MatrixInstance {
                        base_task: name.clone(),
                        attrs: attrs.clone(),
                    },
                );
            }
        } else {
            let mut def = definition.clone();
            def.depends_on = expand_depends_on(&definition.depends_on, &instance_ids);
            expanded.insert(name.clone(), def);
        }
    }

    Ok(MatrixExpansion {
        tasks: expanded,
        instance_ids,
        instances,
    })
}

/// Expand requested roots that refer to matrix tasks into concrete instance ids.
#[must_use]
pub fn expand_matrix_roots(roots: &[&str], expansion: &MatrixExpansion) -> Vec<String> {
    let mut out = Vec::new();
    for root in roots {
        if let Some(ids) = expansion.instance_ids.get(*root) {
            out.extend(ids.iter().cloned());
        } else {
            out.push((*root).to_owned());
        }
    }
    out
}

/// Validate matrix metadata during schema load (schema v2).
pub fn validate_task_matrix(task: &str, matrix: &TaskMatrix) -> Result<(), SchemaError> {
    if matrix.include.is_empty() {
        return Err(SchemaError::InvalidMatrix {
            task: task.to_owned(),
            message: "matrix.include must contain at least one entry".to_owned(),
        });
    }
    Ok(())
}

/// Reject matrix expansion ids (`task@N`) that collide with declared task names.
pub fn validate_matrix_instance_collisions(
    tasks: &BTreeMap<String, TaskDefinition>,
) -> Result<(), SchemaError> {
    let task_names: std::collections::BTreeSet<&str> = tasks.keys().map(String::as_str).collect();
    for (name, definition) in tasks {
        if let Some(matrix) = &definition.matrix {
            for index in 0..matrix.include.len() {
                let instance_id = matrix_node_id(name, index);
                if task_names.contains(instance_id.as_str()) && instance_id != *name {
                    return Err(SchemaError::InvalidMatrix {
                        task: name.clone(),
                        message: format!(
                            "expanded instance id `{instance_id}` collides with declared task name `{instance_id}`"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn expand_depends_on(deps: &[String], instance_ids: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    for dep in deps {
        if let Some(ids) = instance_ids.get(dep) {
            out.extend(ids.iter().cloned());
        } else {
            out.push(dep.clone());
        }
    }
    out
}

fn matrix_node_id(base: &str, index: usize) -> String {
    format!("{base}@{index}")
}

pub fn validate_matrix_attrs(
    task: &str,
    index: usize,
    attrs: &BTreeMap<String, JsonValue>,
) -> Result<(), SchemaError> {
    if attrs.is_empty() {
        return Err(SchemaError::InvalidMatrix {
            task: task.to_owned(),
            message: format!("matrix.include[{index}] must not be empty"),
        });
    }

    let mut seen_normalized = BTreeMap::new();
    for key in attrs.keys() {
        if key.trim().is_empty() {
            return Err(SchemaError::InvalidMatrix {
                task: task.to_owned(),
                message: format!("matrix.include[{index}] key must not be empty"),
            });
        }
        let normalized = key.to_ascii_uppercase();
        if let Some(existing) = seen_normalized.get(&normalized) {
            return Err(SchemaError::InvalidMatrix {
                task: task.to_owned(),
                message: format!(
                    "matrix.include[{index}].{key}: key collides with `{existing}` after normalization to {MATRIX_ENV_PREFIX}{normalized}"
                ),
            });
        }
        seen_normalized.insert(normalized, key.clone());
    }

    for (key, value) in attrs {
        if !is_canonical_ident(key) {
            return Err(SchemaError::InvalidMatrix {
                task: task.to_owned(),
                message: format!("matrix.include[{index}].{key}: key must match [a-z][a-z0-9_]*"),
            });
        }
        if matrix_value_to_string(value).is_none() {
            return Err(SchemaError::InvalidMatrix {
                task: task.to_owned(),
                message: format!(
                    "matrix.include[{index}].{key}: value must be a string, number, or boolean"
                ),
            });
        }
    }
    Ok(())
}

fn matrix_value_to_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => Some(text.clone()),
        JsonValue::Bool(flag) => Some(if *flag { "true" } else { "false" }.to_owned()),
        JsonValue::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TaskMatrix;
    use serde_json::json;

    fn matrix_task(include: Vec<BTreeMap<String, JsonValue>>) -> TaskDefinition {
        let mut task = TaskDefinition::new("demo");
        task.matrix = Some(TaskMatrix { include });
        task
    }

    #[test]
    fn matrix_env_name_uppercases() {
        assert_eq!(matrix_env_name("os"), "NXR_MATRIX_OS");
        assert_eq!(matrix_env_name("buildTarget"), "NXR_MATRIX_BUILDTARGET");
    }

    #[test]
    fn expand_three_includes_produces_three_nodes() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "shard".to_owned(),
            matrix_task(vec![
                BTreeMap::from([
                    ("os".to_owned(), json!("linux")),
                    ("arch".to_owned(), json!("x64")),
                ]),
                BTreeMap::from([
                    ("os".to_owned(), json!("macos")),
                    ("arch".to_owned(), json!("arm64")),
                ]),
                BTreeMap::from([
                    ("os".to_owned(), json!("linux")),
                    ("arch".to_owned(), json!("arm64")),
                ]),
            ]),
        );

        let expansion = expand_matrix_tasks(&tasks).expect("expand");
        assert_eq!(
            expansion.instance_ids["shard"],
            vec!["shard@0", "shard@1", "shard@2"]
        );
        assert_eq!(expansion.tasks.len(), 3);
        assert!(expansion.tasks["shard@0"].matrix.is_none());
        assert_eq!(
            resolve_matrix_env(&expansion.instances["shard@1"].attrs)["NXR_MATRIX_OS"],
            "macos"
        );
    }

    #[test]
    fn depends_on_matrix_task_waits_for_all_instances() {
        let mut tasks = BTreeMap::new();
        tasks.insert("base".to_owned(), TaskDefinition::new("base"));
        tasks.insert(
            "shard".to_owned(),
            matrix_task(vec![
                BTreeMap::from([("os".to_owned(), json!("linux"))]),
                BTreeMap::from([("os".to_owned(), json!("macos"))]),
            ]),
        );
        let mut ci = TaskDefinition::new("ci");
        ci.depends_on = vec!["shard".to_owned()];
        tasks.insert("ci".to_owned(), ci);

        let expansion = expand_matrix_tasks(&tasks).expect("expand");
        assert_eq!(
            expansion.tasks["ci"].depends_on,
            vec!["shard@0".to_owned(), "shard@1".to_owned()]
        );
    }

    #[test]
    fn expand_roots_for_matrix_task() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "shard".to_owned(),
            matrix_task(vec![
                BTreeMap::from([("os".to_owned(), json!("linux"))]),
                BTreeMap::from([("os".to_owned(), json!("macos"))]),
            ]),
        );
        let expansion = expand_matrix_tasks(&tasks).expect("expand");
        assert_eq!(
            expand_matrix_roots(&["shard"], &expansion),
            vec!["shard@0".to_owned(), "shard@1".to_owned()]
        );
    }

    #[test]
    fn rejects_matrix_instance_id_collision_with_task_name() {
        let mut tasks = BTreeMap::new();
        tasks.insert("foo@0".to_owned(), TaskDefinition::new("base"));
        tasks.insert(
            "foo".to_owned(),
            matrix_task(vec![BTreeMap::from([("os".to_owned(), json!("linux"))])]),
        );
        let err = validate_matrix_instance_collisions(&tasks).expect_err("collision");
        assert!(matches!(err, SchemaError::InvalidMatrix { .. }));
        assert!(err.to_string().contains("foo@0"));
    }

    #[test]
    fn rejects_matrix_key_collision_after_normalization() {
        let err = validate_matrix_attrs(
            "demo",
            0,
            &BTreeMap::from([
                ("foo".to_owned(), json!("linux")),
                ("Foo".to_owned(), json!("macos")),
            ]),
        )
        .expect_err("collision");
        assert!(matches!(err, SchemaError::InvalidMatrix { .. }));
        assert!(err.to_string().contains("NXR_MATRIX_FOO"));
    }

    #[test]
    fn rejects_unknown_matrix_shape_at_parse() {
        let err = validate_matrix_attrs(
            "demo",
            0,
            &BTreeMap::from([("mode".to_owned(), json!({ "nested": true }))]),
        )
        .expect_err("object value rejected");
        assert!(matches!(err, SchemaError::InvalidMatrix { .. }));
    }
}
