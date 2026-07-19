//! Pure ready-queue scheduler over an [`ExecutionPlan`].
//!
//! The scheduler does not spawn processes. Callers drive it by:
//! 1. constructing a [`Scheduler`] from a plan and a job limit;
//! 2. calling [`Scheduler::schedule_ready`] to obtain nodes to start;
//! 3. reporting each exit via [`Scheduler::on_exit`], then scheduling again.
//!
//! Ready sets are computed from `dependsOn` edges (not from serial one-node
//! waves). When multiple nodes are ready, the lexicographically smallest ids
//! are started first, up to the in-flight `jobs` cap.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::plan_exec::{ExecutionPlan, FailurePolicy};

/// Errors from constructing or driving a [`Scheduler`].
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SchedulerError {
    /// `jobs` must be at least 1.
    #[error("jobs must be >= 1 (got {jobs})")]
    InvalidJobs { jobs: usize },

    /// `on_exit` was called for a node that is not currently running.
    #[error("node `{node}` is not running")]
    NotRunning { node: String },

    /// `on_exit` referenced a node id absent from the plan.
    #[error("unknown node `{node}`")]
    UnknownNode { node: String },
}

/// Lifecycle state of one plan node inside the scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeState {
    /// Waiting on dependencies.
    Pending,
    /// Dependencies satisfied; waiting for a job slot.
    Ready,
    /// Occupying a job slot (caller owns the actual work).
    Running,
    /// Exited with code 0.
    Succeeded,
    /// Exited with a non-zero code.
    Failed,
    /// Never started: fail-fast cancellation, or blocked by a failed dependency.
    Cancelled,
}

impl NodeState {
    /// Whether this state is terminal (no further transitions).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeState::Succeeded | NodeState::Failed | NodeState::Cancelled
        )
    }
}

/// Final outcome after the scheduler has no more work to schedule or run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleOutcome {
    /// True only when every plan node reached [`NodeState::Succeeded`].
    pub success: bool,
    /// True when fail-fast tripped and remaining work was cancelled.
    pub cancelled: bool,
    /// Nodes that failed (non-zero exit).
    pub failed: Vec<String>,
    /// Nodes cancelled without running.
    pub cancelled_nodes: Vec<String>,
}

/// Ready-queue scheduler with a global in-flight job limit.
#[derive(Clone, Debug)]
pub struct Scheduler {
    failure_policy: FailurePolicy,
    jobs: usize,
    states: BTreeMap<String, NodeState>,
    /// Remaining unsatisfied dependencies (decremented only on success).
    remaining_deps: BTreeMap<String, usize>,
    /// dependency → dependents that list it in `dependsOn`.
    dependents: BTreeMap<String, BTreeSet<String>>,
    ready: BTreeSet<String>,
    running: BTreeSet<String>,
    /// Fail-fast: stop starting new nodes after the first failure.
    fail_fast_tripped: bool,
}

impl Scheduler {
    /// Create a scheduler from an [`ExecutionPlan`].
    ///
    /// Ready sets are derived from each [`crate::plan_exec::PlanNode`]'s
    /// `depends_on` edges. The plan's `failure_policy` is used; `waves` are
    /// ignored so parallel ready-sets work even when the plan was built with
    /// one-node serial waves.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulerError::InvalidJobs`] when `jobs == 0`.
    pub fn new(plan: &ExecutionPlan, jobs: usize) -> Result<Self, SchedulerError> {
        if jobs == 0 {
            return Err(SchedulerError::InvalidJobs { jobs: 0 });
        }

        let mut states = BTreeMap::new();
        let mut remaining_deps = BTreeMap::new();
        let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut ready = BTreeSet::new();

        for node in &plan.nodes {
            states.insert(node.id.clone(), NodeState::Pending);
            remaining_deps.insert(node.id.clone(), node.depends_on.len());
            dependents.entry(node.id.clone()).or_default();
        }

        for node in &plan.nodes {
            for dep in &node.depends_on {
                dependents
                    .entry(dep.clone())
                    .or_default()
                    .insert(node.id.clone());
            }
        }

        for (id, deg) in &remaining_deps {
            if *deg == 0 {
                ready.insert(id.clone());
                states.insert(id.clone(), NodeState::Ready);
            }
        }

        Ok(Self {
            failure_policy: plan.failure_policy,
            jobs,
            states,
            remaining_deps,
            dependents,
            ready,
            running: BTreeSet::new(),
            fail_fast_tripped: false,
        })
    }

    /// Override the failure policy (defaults to the plan's policy).
    #[must_use]
    pub fn with_failure_policy(mut self, policy: FailurePolicy) -> Self {
        self.failure_policy = policy;
        self
    }

    /// Number of allowed concurrent running nodes.
    #[must_use]
    pub fn jobs(&self) -> usize {
        self.jobs
    }

    /// Active failure policy.
    #[must_use]
    pub fn failure_policy(&self) -> FailurePolicy {
        self.failure_policy
    }

    /// Current state of `node`, if present.
    #[must_use]
    pub fn state(&self, node: &str) -> Option<NodeState> {
        self.states.get(node).copied()
    }

    /// Ids currently occupying job slots (lexicographic order).
    pub fn running(&self) -> impl Iterator<Item = &str> {
        self.running.iter().map(String::as_str)
    }

    /// Ids waiting for a job slot (lexicographic order).
    pub fn ready(&self) -> impl Iterator<Item = &str> {
        self.ready.iter().map(String::as_str)
    }

    /// How many nodes are currently running.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.running.len()
    }

    /// Whether fail-fast has tripped (no further starts will be issued).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.fail_fast_tripped
    }

    /// True when nothing is running and nothing more can be started.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.running.is_empty() && self.ready.is_empty()
    }

    /// Start as many ready nodes as free job slots allow.
    ///
    /// Returns newly started node ids in lexicographic start order (ready-set
    /// order). Empty when no slots are free, the ready set is empty, or
    /// fail-fast has cancelled further starts.
    pub fn schedule_ready(&mut self) -> Vec<String> {
        if self.fail_fast_tripped {
            self.cancel_all_not_started();
            return Vec::new();
        }

        let mut started = Vec::new();
        while self.running.len() < self.jobs {
            let Some(id) = self.ready.iter().next().cloned() else {
                break;
            };
            self.ready.remove(&id);
            self.states.insert(id.clone(), NodeState::Running);
            self.running.insert(id.clone());
            started.push(id);
        }
        started
    }

    /// Record that a running node exited with `code`.
    ///
    /// On success, dependents whose remaining dependencies reach zero become
    /// ready. On failure:
    /// - [`FailurePolicy::FailFast`]: signal cancellation and do not start new
    ///   nodes (in-flight work is left to the caller);
    /// - [`FailurePolicy::KeepGoing`]: cancel dependents that required the
    ///   failed node, then continue scheduling unrelated ready work.
    ///
    /// After calling this, invoke [`Self::schedule_ready`] to obtain any newly
    /// startable nodes.
    ///
    /// # Errors
    ///
    /// - [`SchedulerError::UnknownNode`] if `node` is not in the plan
    /// - [`SchedulerError::NotRunning`] if `node` is not currently running
    pub fn on_exit(&mut self, node: &str, code: i32) -> Result<(), SchedulerError> {
        if !self.states.contains_key(node) {
            return Err(SchedulerError::UnknownNode {
                node: node.to_owned(),
            });
        }
        if !self.running.contains(node) {
            return Err(SchedulerError::NotRunning {
                node: node.to_owned(),
            });
        }

        self.running.remove(node);

        if code == 0 {
            self.states.insert(node.to_owned(), NodeState::Succeeded);
            self.unlock_dependents(node);
        } else {
            self.states.insert(node.to_owned(), NodeState::Failed);
            match self.failure_policy {
                FailurePolicy::FailFast => {
                    self.fail_fast_tripped = true;
                    self.cancel_all_not_started();
                }
                FailurePolicy::KeepGoing => {
                    self.cancel_transitive_dependents(node);
                }
            }
        }

        Ok(())
    }

    /// Convenience: [`Self::on_exit`] then [`Self::schedule_ready`].
    ///
    /// # Errors
    ///
    /// Same as [`Self::on_exit`].
    pub fn complete(&mut self, node: &str, code: i32) -> Result<Vec<String>, SchedulerError> {
        self.on_exit(node, code)?;
        Ok(self.schedule_ready())
    }

    /// Snapshot the outcome once [`Self::is_finished`] is true.
    ///
    /// Meaningful earlier, but pending/ready/running nodes are neither failed
    /// nor cancelled yet.
    #[must_use]
    pub fn outcome(&self) -> ScheduleOutcome {
        let mut failed = Vec::new();
        let mut cancelled_nodes = Vec::new();
        let mut all_succeeded = true;

        for (id, state) in &self.states {
            match state {
                NodeState::Succeeded => {}
                NodeState::Failed => {
                    all_succeeded = false;
                    failed.push(id.clone());
                }
                NodeState::Cancelled => {
                    all_succeeded = false;
                    cancelled_nodes.push(id.clone());
                }
                NodeState::Pending | NodeState::Ready | NodeState::Running => {
                    all_succeeded = false;
                }
            }
        }

        ScheduleOutcome {
            success: all_succeeded,
            cancelled: self.fail_fast_tripped,
            failed,
            cancelled_nodes,
        }
    }

    fn unlock_dependents(&mut self, node: &str) {
        let Some(children) = self.dependents.get(node).cloned() else {
            return;
        };
        for child in children {
            let Some(state) = self.states.get(&child).copied() else {
                continue;
            };
            if state != NodeState::Pending {
                continue;
            }
            let deg = self
                .remaining_deps
                .get_mut(&child)
                .expect("node in remaining_deps");
            *deg = deg.saturating_sub(1);
            if *deg == 0 {
                self.ready.insert(child.clone());
                self.states.insert(child, NodeState::Ready);
            }
        }
    }

    fn cancel_all_not_started(&mut self) {
        let to_cancel: Vec<String> = self
            .states
            .iter()
            .filter(|&(_, state)| matches!(state, NodeState::Pending | NodeState::Ready))
            .map(|(id, _)| id.clone())
            .collect();
        for id in to_cancel {
            self.ready.remove(&id);
            self.states.insert(id, NodeState::Cancelled);
        }
    }

    /// Cancel every node that transitively depends on `failed` (`KeepGoing`).
    fn cancel_transitive_dependents(&mut self, failed: &str) {
        let mut stack: Vec<String> = self
            .dependents
            .get(failed)
            .into_iter()
            .flat_map(|s| s.iter().cloned())
            .collect();
        let mut seen = BTreeSet::new();

        while let Some(id) = stack.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(state) = self.states.get(&id).copied() else {
                continue;
            };
            match state {
                NodeState::Pending | NodeState::Ready => {
                    self.ready.remove(&id);
                    self.states.insert(id.clone(), NodeState::Cancelled);
                    if let Some(children) = self.dependents.get(&id) {
                        stack.extend(children.iter().cloned());
                    }
                }
                // Running/Succeeded/Failed/Cancelled: do not retroactively cancel.
                NodeState::Running
                | NodeState::Succeeded
                | NodeState::Failed
                | NodeState::Cancelled => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_exec::{FailurePolicy, build_execution_plan, build_serial_plan};
    use crate::schema::TaskDefinition;
    use std::collections::BTreeMap;

    fn task(deps: &[&str]) -> TaskDefinition {
        let mut def = TaskDefinition::new("app");
        def.depends_on = deps.iter().map(|s| (*s).to_owned()).collect();
        def
    }

    fn diamond() -> BTreeMap<String, TaskDefinition> {
        let mut tasks = BTreeMap::new();
        tasks.insert("a".to_owned(), task(&[]));
        tasks.insert("b".to_owned(), task(&["a"]));
        tasks.insert("c".to_owned(), task(&["a"]));
        tasks.insert("d".to_owned(), task(&["b", "c"]));
        tasks
    }

    #[test]
    fn large_linear_dag_schedules_successfully() {
        const NODE_COUNT: usize = 300;
        let mut tasks = BTreeMap::new();
        tasks.insert("n0".to_owned(), task(&[]));
        for index in 1..NODE_COUNT {
            let prev = format!("n{}", index - 1);
            let id = format!("n{index}");
            tasks.insert(id, task(&[prev.as_str()]));
        }
        let root = format!("n{}", NODE_COUNT - 1);
        let plan = build_serial_plan(&tasks, &root).expect("plan");
        assert_eq!(plan.nodes.len(), NODE_COUNT);

        let mut sched = Scheduler::new(&plan, 4).expect("sched");
        let mut pending = sched.schedule_ready();
        while !sched.is_finished() {
            while let Some(id) = pending.pop() {
                pending.extend(sched.complete(&id, 0).expect("complete"));
            }
            pending.extend(sched.schedule_ready());
            assert!(
                !pending.is_empty() || sched.is_finished(),
                "scheduler stalled with no ready or running work"
            );
        }
        assert!(sched.outcome().success);
        assert_eq!(sched.outcome().failed.len(), 0);
    }

    #[test]
    fn rejects_zero_jobs() {
        let plan = build_serial_plan(&diamond(), "d").expect("plan");
        let err = Scheduler::new(&plan, 0).expect_err("jobs");
        assert_eq!(err, SchedulerError::InvalidJobs { jobs: 0 });
    }

    #[test]
    fn diamond_jobs_2_shows_concurrent_capacity() {
        let plan = build_serial_plan(&diamond(), "d").expect("plan");
        // Serial waves are one-node; scheduler must still parallelize from edges.
        assert_eq!(plan.waves.len(), 4);

        let mut sched = Scheduler::new(&plan, 2).expect("sched");
        assert_eq!(sched.schedule_ready(), vec!["a".to_owned()]);
        assert_eq!(sched.in_flight(), 1);

        let next = sched.complete("a", 0).expect("a ok");
        assert_eq!(next, vec!["b".to_owned(), "c".to_owned()]);
        assert_eq!(sched.in_flight(), 2);
        assert_eq!(sched.running().collect::<Vec<_>>(), vec!["b", "c"]);

        assert!(sched.complete("b", 0).expect("b ok").is_empty());
        assert_eq!(sched.in_flight(), 1);

        let next = sched.complete("c", 0).expect("c ok");
        assert_eq!(next, vec!["d".to_owned()]);
        assert!(sched.complete("d", 0).expect("d ok").is_empty());

        assert!(sched.is_finished());
        assert!(sched.outcome().success);
    }

    #[test]
    fn diamond_jobs_1_is_serial_lex_order() {
        let plan = build_serial_plan(&diamond(), "d").expect("plan");
        let mut sched = Scheduler::new(&plan, 1).expect("sched");

        assert_eq!(sched.schedule_ready(), vec!["a".to_owned()]);
        assert_eq!(sched.complete("a", 0).expect("a"), vec!["b".to_owned()]);
        assert_eq!(sched.complete("b", 0).expect("b"), vec!["c".to_owned()]);
        assert_eq!(sched.complete("c", 0).expect("c"), vec!["d".to_owned()]);
        assert!(sched.complete("d", 0).expect("d").is_empty());

        assert!(sched.is_finished());
        assert_eq!(
            [
                sched.state("a"),
                sched.state("b"),
                sched.state("c"),
                sched.state("d"),
            ],
            [
                Some(NodeState::Succeeded),
                Some(NodeState::Succeeded),
                Some(NodeState::Succeeded),
                Some(NodeState::Succeeded),
            ]
        );
    }

    #[test]
    fn fail_fast_stops_further_starts() {
        let plan =
            build_execution_plan(&diamond(), "d", FailurePolicy::FailFast, None).expect("plan");
        let mut sched = Scheduler::new(&plan, 2).expect("sched");

        assert_eq!(sched.schedule_ready(), vec!["a".to_owned()]);
        assert_eq!(
            sched.complete("a", 0).expect("a"),
            vec!["b".to_owned(), "c".to_owned()]
        );

        // b fails while c is in-flight: no new starts; d is cancelled.
        assert!(sched.complete("b", 1).expect("b fail").is_empty());
        assert!(sched.is_cancelled());
        assert_eq!(sched.state("d"), Some(NodeState::Cancelled));
        assert_eq!(sched.state("c"), Some(NodeState::Running));

        // In-flight c may still finish; still no starts.
        assert!(sched.complete("c", 0).expect("c").is_empty());
        assert!(sched.is_finished());

        let outcome = sched.outcome();
        assert!(!outcome.success);
        assert!(outcome.cancelled);
        assert_eq!(outcome.failed, vec!["b".to_owned()]);
        assert_eq!(outcome.cancelled_nodes, vec!["d".to_owned()]);
    }

    #[test]
    fn keep_going_continues_unrelated_ready_work() {
        let plan =
            build_execution_plan(&diamond(), "d", FailurePolicy::KeepGoing, None).expect("plan");
        let mut sched = Scheduler::new(&plan, 1).expect("sched");

        assert_eq!(sched.schedule_ready(), vec!["a".to_owned()]);
        assert_eq!(sched.complete("a", 0).expect("a"), vec!["b".to_owned()]);

        // b fails; d (depends on b) is cancelled, but c can still run.
        assert_eq!(
            sched.complete("b", 1).expect("b fail"),
            vec!["c".to_owned()]
        );
        assert_eq!(sched.state("d"), Some(NodeState::Cancelled));
        assert!(!sched.is_cancelled());

        assert!(sched.complete("c", 0).expect("c").is_empty());
        assert!(sched.is_finished());

        let outcome = sched.outcome();
        assert!(!outcome.success);
        assert!(!outcome.cancelled);
        assert_eq!(outcome.failed, vec!["b".to_owned()]);
        assert_eq!(outcome.cancelled_nodes, vec!["d".to_owned()]);
    }

    #[test]
    fn keep_going_with_jobs_2_cancels_blocked_join() {
        let plan =
            build_execution_plan(&diamond(), "d", FailurePolicy::KeepGoing, None).expect("plan");
        let mut sched = Scheduler::new(&plan, 2).expect("sched");

        sched.schedule_ready();
        sched.complete("a", 0).expect("a");
        // b and c both running
        assert!(sched.complete("b", 1).expect("b fail").is_empty());
        assert_eq!(sched.state("d"), Some(NodeState::Cancelled));
        // c still finishes; d stays cancelled
        assert!(sched.complete("c", 0).expect("c").is_empty());
        assert!(sched.is_finished());
        assert_eq!(sched.state("d"), Some(NodeState::Cancelled));
    }

    #[test]
    fn on_exit_rejects_unknown_and_not_running() {
        let plan = build_serial_plan(&diamond(), "d").expect("plan");
        let mut sched = Scheduler::new(&plan, 2).expect("sched");
        sched.schedule_ready();

        assert_eq!(
            sched.on_exit("ghost", 0).expect_err("unknown"),
            SchedulerError::UnknownNode {
                node: "ghost".to_owned(),
            }
        );
        assert_eq!(
            sched.on_exit("b", 0).expect_err("not running"),
            SchedulerError::NotRunning {
                node: "b".to_owned(),
            }
        );
    }

    #[test]
    fn with_failure_policy_override() {
        let plan = build_serial_plan(&diamond(), "d").expect("plan");
        assert_eq!(plan.failure_policy, FailurePolicy::FailFast);

        let mut sched = Scheduler::new(&plan, 1)
            .expect("sched")
            .with_failure_policy(FailurePolicy::KeepGoing);
        assert_eq!(sched.failure_policy(), FailurePolicy::KeepGoing);

        sched.schedule_ready();
        sched.complete("a", 0).expect("a");
        sched.complete("b", 1).expect("b fail");
        // KeepGoing override: c still starts.
        assert_eq!(sched.state("c"), Some(NodeState::Running));
    }
}
