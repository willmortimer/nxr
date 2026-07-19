//! Serial task planning over a dependency DAG.
//!
//! [`plan_serial`] returns a deterministic topological order for the subgraph
//! reachable as dependencies of a chosen root (ancestors plus the root). When
//! multiple nodes are ready, the lexicographically smallest id is chosen next
//! (Kahn with a [`BTreeSet`] ready set).

use std::collections::{BTreeMap, BTreeSet};

use crate::graph::{GraphError, TaskGraph};
use crate::schema::TaskDefinition;

/// Planning / graph validation errors (alias of [`GraphError`]).
///
/// Intended to map to exit code 8 (`TASK_GRAPH`) at the CLI boundary.
pub type PlanError = GraphError;

/// Compute a deterministic serial execution order for `root`.
///
/// The result includes only `root` and its transitive `dependsOn` ancestors.
/// Dependencies appear before dependents; `root` is last when the graph is valid.
///
/// # Errors
///
/// - [`PlanError::UnknownRoot`] when `root` is not in `tasks`
/// - [`PlanError::MissingDependency`] when a reachable task references an
///   undefined dependency
/// - [`PlanError::Cycle`] when the subgraph contains a cycle (path included)
pub fn plan_serial(
    tasks: &BTreeMap<String, TaskDefinition>,
    root: &str,
) -> Result<Vec<String>, PlanError> {
    let graph = TaskGraph::subgraph(tasks, root)?;
    topo_serial(&graph)
}

/// Kahn topological sort with lexicographic tie-breaking.
fn topo_serial(graph: &TaskGraph) -> Result<Vec<String>, PlanError> {
    // dependents: dependency → sorted dependents that list it in dependsOn
    let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();

    for id in graph.node_ids() {
        indegree.insert(id.to_owned(), 0);
        dependents.entry(id.to_owned()).or_default();
    }

    for (dependent, deps) in graph
        .node_ids()
        .filter_map(|id| graph.dependencies(id).map(|d| (id, d)))
    {
        for dep in deps {
            indegree.entry(dependent.to_owned()).and_modify(|n| *n += 1);
            dependents
                .entry(dep.clone())
                .or_default()
                .insert(dependent.to_owned());
        }
    }

    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|&(_, deg)| *deg == 0)
        .map(|(id, _)| id.clone())
        .collect();

    let mut order = Vec::with_capacity(graph.len());
    while let Some(id) = ready.iter().next().cloned() {
        ready.remove(&id);
        order.push(id.clone());
        if let Some(children) = dependents.get(&id) {
            for child in children {
                let deg = indegree.get_mut(child).expect("node in indegree");
                *deg -= 1;
                if *deg == 0 {
                    ready.insert(child.clone());
                }
            }
        }
    }

    if order.len() != graph.len() {
        let remaining: BTreeSet<String> = indegree
            .into_iter()
            .filter(|(_, deg)| *deg > 0)
            .map(|(id, _)| id)
            .collect();
        let path = find_cycle_path(graph, &remaining);
        return Err(PlanError::Cycle { path });
    }

    Ok(order)
}

/// Find one cycle path among `remaining` nodes; path starts and ends at the same id.
fn find_cycle_path(graph: &TaskGraph, remaining: &BTreeSet<String>) -> Vec<String> {
    // DFS following dependsOn edges within the remaining set.
    let mut visiting = BTreeSet::new();
    let mut stack: Vec<String> = Vec::new();

    for start in remaining {
        if let Some(path) = dfs_cycle(graph, remaining, start, &mut visiting, &mut stack) {
            return path;
        }
        visiting.clear();
        stack.clear();
    }

    // Fallback: should be unreachable if Kahn left nodes with indegree > 0.
    remaining.iter().cloned().collect()
}

fn dfs_cycle(
    graph: &TaskGraph,
    remaining: &BTreeSet<String>,
    node: &str,
    visiting: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    if !remaining.contains(node) {
        return None;
    }
    if visiting.contains(node) {
        let start = stack.iter().position(|n| n == node).unwrap_or(0);
        let mut path: Vec<String> = stack[start..].to_vec();
        path.push(node.to_owned());
        return Some(path);
    }

    visiting.insert(node.to_owned());
    stack.push(node.to_owned());

    if let Some(deps) = graph.dependencies(node) {
        for dep in deps {
            if !remaining.contains(dep) {
                continue;
            }
            if let Some(path) = dfs_cycle(graph, remaining, dep, visiting, stack) {
                return Some(path);
            }
        }
    }

    stack.pop();
    visiting.remove(node);
    None
}

/// Convenience: plan then render text for a root.
///
/// # Errors
///
/// Returns the same errors as [`plan_serial`].
pub fn plan_text(
    tasks: &BTreeMap<String, TaskDefinition>,
    root: &str,
) -> Result<String, PlanError> {
    let order = plan_serial(tasks, root)?;
    Ok(crate::graph::render_text(&order))
}

/// Convenience: subgraph Mermaid for a root after validating a serial plan.
///
/// # Errors
///
/// Returns the same errors as [`plan_serial`].
pub fn plan_mermaid(
    tasks: &BTreeMap<String, TaskDefinition>,
    root: &str,
) -> Result<String, PlanError> {
    // Validate acyclicity via planning, then render the same subgraph.
    let _order = plan_serial(tasks, root)?;
    let graph = TaskGraph::subgraph(tasks, root)?;
    Ok(crate::graph::render_mermaid(&graph))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{render_mermaid, render_text};
    use crate::schema::TaskDefinition;

    fn task(deps: &[&str]) -> TaskDefinition {
        let mut def = TaskDefinition::new("app");
        def.depends_on = deps.iter().map(|s| (*s).to_owned()).collect();
        def
    }

    #[test]
    fn diamond_deterministic_serial_order() {
        //   a
        //  / \
        // b   c
        //  \ /
        //   d
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["a"]));
        tasks.insert("d".to_owned(), task(&["b", "c"]));

        let order = plan_serial(&tasks, "d").expect("plan");
        assert_eq!(order, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn diamond_lexicographic_tie_break() {
        // Both x and y ready after a; lex order picks x before y.
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("y".to_owned(), task(&["a"]));
        tasks.insert("x".to_owned(), task(&["a"]));
        tasks.insert("z".to_owned(), task(&["x", "y"]));

        let order = plan_serial(&tasks, "z").expect("plan");
        assert_eq!(order, vec!["a", "x", "y", "z"]);
    }

    #[test]
    fn cycle_returns_path() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&["c"]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["b"]));

        let err = plan_serial(&tasks, "c").expect_err("cycle");
        match err {
            PlanError::Cycle { path } => {
                assert!(path.len() >= 2, "path: {path:?}");
                assert_eq!(path.first(), path.last());
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn self_cycle() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&["a"]));
        let err = plan_serial(&tasks, "a").expect_err("self cycle");
        assert!(matches!(err, PlanError::Cycle { .. }));
    }

    #[test]
    fn unknown_root() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        let err = plan_serial(&tasks, "missing").expect_err("unknown");
        assert_eq!(
            err,
            PlanError::UnknownRoot {
                root: "missing".to_owned(),
            }
        );
    }

    #[test]
    fn missing_depends_on_target() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&["ghost"]));
        let err = plan_serial(&tasks, "a").expect_err("missing dep");
        assert_eq!(
            err,
            PlanError::MissingDependency {
                task: "a".to_owned(),
                dependency: "ghost".to_owned(),
            }
        );
    }

    #[test]
    fn plan_ignores_unrelated_tasks() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("unrelated".to_owned(), task(&[]));
        let order = plan_serial(&tasks, "b").expect("plan");
        assert_eq!(order, vec!["a", "b"]);
    }

    #[test]
    fn text_and_mermaid_snapshots() {
        let mut tasks = BTreeMap::new();
        tasks.insert("fmt".to_owned(), task(&[]));
        tasks.insert("lint".to_owned(), task(&[]));
        tasks.insert("test".to_owned(), task(&["fmt", "lint"]));

        let order = plan_serial(&tasks, "test").expect("plan");
        assert_eq!(order, vec!["fmt", "lint", "test"]);
        assert_eq!(render_text(&order), "fmt\nlint\ntest\n");

        let graph = TaskGraph::subgraph(&tasks, "test").expect("graph");
        assert_eq!(
            render_mermaid(&graph),
            concat!(
                "flowchart TD\n",
                "  \"fmt\"\n",
                "  \"lint\"\n",
                "  \"test\"\n",
                "  \"fmt\" --> \"test\"\n",
                "  \"lint\" --> \"test\"\n",
            )
        );
    }

    #[test]
    fn plan_mermaid_rejects_cycle() {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&["b"]));
        tasks.insert("b".to_owned(), task(&["a"]));
        assert!(matches!(
            plan_mermaid(&tasks, "a"),
            Err(PlanError::Cycle { .. })
        ));
    }
}
