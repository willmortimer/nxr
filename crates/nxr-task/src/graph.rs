//! Task DAG construction and stable text/Mermaid/DOT rendering.
//!
//! Edges follow `dependsOn`: if task `A` lists `B`, then `B` must run before `A`.
//! Renderers emit dependency-precedence edges (`dep --> dependent`) with
//! deterministic node and edge ordering via [`BTreeMap`] / sorted iteration.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::schema::TaskDefinition;

/// Errors while building or validating a task dependency graph.
///
/// Intended to map to exit code 8 (`TASK_GRAPH`) at the CLI boundary.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum GraphError {
    /// Requested root task is not present in the task map.
    #[error("unknown task root `{root}`")]
    UnknownRoot { root: String },

    /// A task lists a `dependsOn` entry that is not defined.
    #[error("task `{task}` depends on unknown task `{dependency}`")]
    MissingDependency { task: String, dependency: String },

    /// Dependency edges form a cycle; `path` starts and ends at the same node.
    #[error("task dependency cycle: {}", format_cycle_path(.path))]
    Cycle { path: Vec<String> },
}

fn format_cycle_path(path: &[String]) -> String {
    path.join(" -> ")
}

/// A dependency DAG over a set of task ids.
///
/// Each node maps to a sorted list of tasks it directly depends on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskGraph {
    /// node id → sorted direct dependencies (`dependsOn` targets).
    deps: BTreeMap<String, Vec<String>>,
}

impl TaskGraph {
    /// Build a graph of every task in `tasks`, validating dependency targets.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::MissingDependency`] when any `dependsOn` target is
    /// absent from `tasks`.
    pub fn from_tasks(tasks: &BTreeMap<String, TaskDefinition>) -> Result<Self, GraphError> {
        let mut deps = BTreeMap::new();
        for (name, def) in tasks {
            let mut node_deps = BTreeSet::new();
            for dep in &def.depends_on {
                if !tasks.contains_key(dep) {
                    return Err(GraphError::MissingDependency {
                        task: name.clone(),
                        dependency: dep.clone(),
                    });
                }
                node_deps.insert(dep.clone());
            }
            deps.insert(name.clone(), node_deps.into_iter().collect());
        }
        Ok(Self { deps })
    }

    /// Build the subgraph of `root` and all transitive dependencies.
    ///
    /// # Errors
    ///
    /// - [`GraphError::UnknownRoot`] when `root` is not in `tasks`
    /// - [`GraphError::MissingDependency`] when a reachable task references an
    ///   undefined dependency
    pub fn subgraph(
        tasks: &BTreeMap<String, TaskDefinition>,
        root: &str,
    ) -> Result<Self, GraphError> {
        if !tasks.contains_key(root) {
            return Err(GraphError::UnknownRoot {
                root: root.to_owned(),
            });
        }

        let mut reachable = BTreeSet::new();
        let mut stack = vec![root.to_owned()];
        while let Some(name) = stack.pop() {
            if !reachable.insert(name.clone()) {
                continue;
            }
            match tasks.get(&name) {
                Some(def) => {
                    for dep in &def.depends_on {
                        if !tasks.contains_key(dep) {
                            return Err(GraphError::MissingDependency {
                                task: name.clone(),
                                dependency: dep.clone(),
                            });
                        }
                        stack.push(dep.clone());
                    }
                }
                None => {
                    return Err(GraphError::MissingDependency {
                        task: root.to_owned(),
                        dependency: name,
                    });
                }
            }
        }

        let mut deps = BTreeMap::new();
        for name in &reachable {
            let mut node_deps = BTreeSet::new();
            if let Some(def) = tasks.get(name) {
                for dep in &def.depends_on {
                    node_deps.insert(dep.clone());
                }
            }
            deps.insert(name.clone(), node_deps.into_iter().collect());
        }
        Ok(Self { deps })
    }

    /// Build the union of subgraphs for each root and all transitive dependencies.
    ///
    /// Shared ancestors appear once in the resulting graph.
    ///
    /// # Errors
    ///
    /// - [`GraphError::UnknownRoot`] when any root is not in `tasks`
    /// - [`GraphError::MissingDependency`] when a reachable task references an
    ///   undefined dependency
    pub fn subgraph_union(
        tasks: &BTreeMap<String, TaskDefinition>,
        roots: &[&str],
    ) -> Result<Self, GraphError> {
        if roots.is_empty() {
            return Err(GraphError::UnknownRoot {
                root: String::new(),
            });
        }

        for root in roots {
            if !tasks.contains_key(*root) {
                return Err(GraphError::UnknownRoot {
                    root: (*root).to_owned(),
                });
            }
        }

        let mut reachable = BTreeSet::new();
        for root in roots {
            let mut stack = vec![(*root).to_owned()];
            while let Some(name) = stack.pop() {
                if !reachable.insert(name.clone()) {
                    continue;
                }
                let Some(def) = tasks.get(&name) else {
                    return Err(GraphError::UnknownRoot {
                        root: name,
                    });
                };
                for dep in &def.depends_on {
                    if !tasks.contains_key(dep) {
                        return Err(GraphError::MissingDependency {
                            task: name.clone(),
                            dependency: dep.clone(),
                        });
                    }
                    stack.push(dep.clone());
                }
            }
        }

        let mut deps = BTreeMap::new();
        for name in &reachable {
            let mut node_deps = BTreeSet::new();
            if let Some(def) = tasks.get(name) {
                for dep in &def.depends_on {
                    node_deps.insert(dep.clone());
                }
            }
            deps.insert(name.clone(), node_deps.into_iter().collect());
        }
        Ok(Self { deps })
    }

    /// Task ids in this graph, sorted lexicographically.
    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.deps.keys().map(String::as_str)
    }

    /// Direct dependencies of `id`, or `None` if `id` is not in the graph.
    #[must_use]
    pub fn dependencies(&self, id: &str) -> Option<&[String]> {
        self.deps.get(id).map(Vec::as_slice)
    }

    /// Number of nodes in the graph.
    #[must_use]
    pub fn len(&self) -> usize {
        self.deps.len()
    }

    /// Returns true when the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deps.is_empty()
    }

    /// Stable sorted edge pairs `(dependency, dependent)`.
    ///
    /// Each pair means `dependency` must run before `dependent`.
    #[must_use]
    pub fn precedence_edges(&self) -> Vec<(String, String)> {
        let mut edges = Vec::new();
        for (dependent, deps) in &self.deps {
            for dep in deps {
                edges.push((dep.clone(), dependent.clone()));
            }
        }
        edges.sort();
        edges
    }
}

/// Render a serial plan as a stable newline-separated list of task ids.
///
/// Empty plans yield an empty string. Non-empty plans end with a trailing newline.
#[must_use]
pub fn render_text(order: &[String]) -> String {
    if order.is_empty() {
        return String::new();
    }
    let mut out = order.join("\n");
    out.push('\n');
    out
}

/// Render a Mermaid `flowchart TD` for `graph` with stable node and edge order.
///
/// Edges are `dependency --> dependent` (run order left-to-right along arrows).
#[must_use]
pub fn render_mermaid(graph: &TaskGraph) -> String {
    let mut out = String::from("flowchart TD\n");
    for id in graph.node_ids() {
        out.push_str("  ");
        out.push_str(&mermaid_node_id(id));
        out.push('\n');
    }
    for (dep, dependent) in graph.precedence_edges() {
        out.push_str("  ");
        out.push_str(&mermaid_node_id(&dep));
        out.push_str(" --> ");
        out.push_str(&mermaid_node_id(&dependent));
        out.push('\n');
    }
    out
}

/// Quote Mermaid node ids so task names with special characters stay valid.
fn mermaid_node_id(id: &str) -> String {
    // Mermaid allows quoted ids; always quote for stable, safe output.
    format!("\"{}\"", id.replace('"', "#quot;"))
}

/// Render a Graphviz DOT `digraph` for `graph` with stable node and edge order.
///
/// Edges are `dependency -> dependent` (run order along arrows). Does not invoke
/// Graphviz; output is suitable for `dot -Tpng` or similar tools.
#[must_use]
pub fn render_dot(graph: &TaskGraph) -> String {
    let mut out = String::from("digraph {\n");
    out.push_str("  rankdir=TD;\n");
    for id in graph.node_ids() {
        out.push_str("  ");
        out.push_str(&dot_node_id(id));
        out.push_str(";\n");
    }
    for (dep, dependent) in graph.precedence_edges() {
        out.push_str("  ");
        out.push_str(&dot_node_id(&dep));
        out.push_str(" -> ");
        out.push_str(&dot_node_id(&dependent));
        out.push_str(";\n");
    }
    out.push('}');
    out.push('\n');
    out
}

/// Quote DOT node ids so task names with special characters stay valid.
fn dot_node_id(id: &str) -> String {
    format!("\"{}\"", id.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TaskDefinition;

    fn task(deps: &[&str]) -> TaskDefinition {
        let mut def = TaskDefinition::new("app");
        def.depends_on = deps.iter().map(|s| (*s).to_owned()).collect();
        def
    }

    #[test]
    fn from_tasks_rejects_missing_dependency() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&["missing"]));
        let err = TaskGraph::from_tasks(&tasks).expect_err("missing dep");
        assert_eq!(
            err,
            GraphError::MissingDependency {
                task: "a".to_owned(),
                dependency: "missing".to_owned(),
            }
        );
    }

    #[test]
    fn subgraph_unknown_root() {
        let tasks = BTreeMap::new();
        let err = TaskGraph::subgraph(&tasks, "ci").expect_err("unknown root");
        assert_eq!(
            err,
            GraphError::UnknownRoot {
                root: "ci".to_owned(),
            }
        );
    }

    #[test]
    fn subgraph_union_dedupes_shared_ancestor() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["a"]));
        let graph = TaskGraph::subgraph_union(&tasks, &["b", "c"]).expect("graph");
        assert_eq!(graph.len(), 3);
        assert_eq!(
            graph.node_ids().collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn render_text_stable_plan_lines() {
        let order = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(render_text(&order), "a\nb\nc\n");
        assert_eq!(render_text(&[]), "");
    }

    #[test]
    fn render_dot_stable_diamond() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["a"]));
        tasks.insert("d".to_owned(), task(&["b", "c"]));
        let graph = TaskGraph::subgraph(&tasks, "d").expect("graph");
        let rendered = render_dot(&graph);
        assert_eq!(
            rendered,
            concat!(
                "digraph {\n",
                "  rankdir=TD;\n",
                "  \"a\";\n",
                "  \"b\";\n",
                "  \"c\";\n",
                "  \"d\";\n",
                "  \"a\" -> \"b\";\n",
                "  \"a\" -> \"c\";\n",
                "  \"b\" -> \"d\";\n",
                "  \"c\" -> \"d\";\n",
                "}\n",
            )
        );
    }

    #[test]
    fn render_mermaid_stable_diamond() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["a"]));
        tasks.insert("d".to_owned(), task(&["b", "c"]));
        let graph = TaskGraph::subgraph(&tasks, "d").expect("graph");
        let rendered = render_mermaid(&graph);
        assert_eq!(
            rendered,
            concat!(
                "flowchart TD\n",
                "  \"a\"\n",
                "  \"b\"\n",
                "  \"c\"\n",
                "  \"d\"\n",
                "  \"a\" --> \"b\"\n",
                "  \"a\" --> \"c\"\n",
                "  \"b\" --> \"d\"\n",
                "  \"c\" --> \"d\"\n",
            )
        );
    }
}
