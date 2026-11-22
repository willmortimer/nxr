//! Task schema, graph planning, and scheduling for nxr.
//!
//! - [`schema`] — versioned V1 task document contract
//! - [`graph`] — dependency DAG construction and text/Mermaid/DOT rendering
//! - [`planner`] — deterministic serial topological plans
//! - [`plan_exec`] — versioned [`ExecutionPlan`] envelope
//! - [`workflows`] — optional `.nxr/workflows.toml` operation graph
//! - [`execute`] — operation dispatch (native argv, nix availability, winbuild)
//! - [`events`] — typed execution event bus (`Event` / [`EventSink`])
//! - [`scheduler`] — ready-queue scheduler with job limit ([`Scheduler`])

pub mod action_key;
pub mod artifact;
pub mod availability;
pub mod cache_reason;
pub mod catalog;
pub mod ci_metadata;
pub mod coalesce;
pub mod compiler_cache;
pub mod context;
pub mod control;
pub mod dirty_freeze;
pub mod duration;
pub mod economics;
pub mod events;
pub mod execute;
pub mod execution_spec;
pub mod failure;
pub mod graph;
pub mod idempotency;
pub mod identity;
pub mod matrix;
pub mod memory;
pub mod parameters;
pub mod plan_exec;
pub mod planner;
pub mod process;
pub mod progress;
pub mod qualification;
pub mod receipt;
pub mod remote_execution;
pub mod remote_journal;
pub mod remote_receipt;
pub mod resolve;
pub mod resources;
pub mod run_diagnostics;
pub mod run_events;
pub mod run_journal;
pub mod run_provenance;
pub mod scheduler;
pub mod schema;
pub mod secrets;
pub mod selectors;
pub mod typed_inputs;
pub mod validation_plan;
pub mod workflows;

pub use action_key::{
    WorkspaceCachePlan, WorkspaceCachePlanOptions, build_workspace_cache_plan,
    declared_input_digests,
};
pub use availability::{
    ArtifactAvailability, ArtifactReuse, Availability, CurrentRequirements, ResolvedProgram,
    ReuseDecision, current_requirements, record_producer_receipt, requirements_for_plan,
    resolve_availability, restore_recorded_output, reuse_decision, single_output_paths,
};
pub use cache_reason::{ContextIdentity, cache_reason, context_fingerprint};
pub use catalog::{
    Declaration, DuplicateOperation, EffectClass, EnvironmentExplanation, EnvironmentMismatch,
    EnvironmentView, FingerprintDecision, OperationKind, OperationPlan, OperationRecord,
    OperationSuggestion, PolicySource, ProjectExecutionPolicy, WorkspaceInstance,
    apply_project_policy, describe, discover_members, execution_decision, execution_fingerprint,
    execution_fingerprint_with_inputs, explain, fingerprint_decision, operation_record, plan,
    remote_allowed, search, suggest, validate_policy_parameters, workspace_id,
    workspace_instance_from_checkout,
};
pub use ci_metadata::{CiRunMetadata, attach_ci_run};
pub use context::{
    AppliedTaskContext, ContextError, NXR_ASSUME_YES_ENV, PlanSecretEntry,
    PlanSecretValuePlaceholder, apply_task_context, enforce_context_confirm,
    merge_spawn_env_overrides, resolve_env_provider_secrets, resolve_env_provider_secrets_with,
    resolve_task_context, secret_delivery_mode, secret_provider_mode,
    serialized_plan_excludes_value,
};
pub use control::{
    CONTROL_V1, ControlOutcome, DiagnosticSeverity, UntrustedArtifactDeclaration,
    UntrustedDiagnostic, parse_control_message,
};
pub use dirty_freeze::{freeze_dirty_manifest, frozen_manifest};
pub use duration::{format_duration, parse_duration};
pub use events::{
    ChunkEncoding, Event, EventSink, KNOWN_PHASES, NodeOutcome, NullSink, OutputPayload,
    RecordingSink, RunOutcome, event_kind, known_phase,
};
pub use execute::{
    ExecContext, ExecuteError, ExecutorName, OperationResult, OperationRun, UNAVAILABLE_EXECUTOR,
    execute_operation, run_operation_plan,
};
pub use execution_spec::{
    AdmissionPolicy, CiIdentity, ExecutionSpecV2, OutputDecl, RunEventV2, SourceBinding, SpecRun,
    bind_source_members, execute_spec_locally, execution_spec_for_task,
    execution_specs_for_document, source_set_from_repo, spec_with_source_set,
    workflow_definition_digest,
};
pub use failure::{FailureSummary, failure_summary};
pub use graph::{GraphError, TaskGraph, render_dot, render_mermaid, render_text};
pub use idempotency::{
    Admission, CoalesceOutcome, CoalesceRequest, DetachOutcome, Reservation, admit,
    attach_observer, bind_supervisor, cancel, detach_observer_id, finish_coalesce,
    idempotency_key_for_run, inspect, mark_admitting, mark_bind_incomplete, mark_reconciliation,
    mark_started,
};
pub use identity::{
    SOURCE_SET_V2, SourceMemberV2, SourceSetV1, SourceSetV2, operation_id, project_primary_v1,
    source_set_digest, source_set_digest_v2,
};
pub use matrix::{
    MATRIX_ENV_PREFIX, MatrixExpansion, MatrixInstance, expand_matrix_roots, expand_matrix_tasks,
    matrix_attr_names, matrix_env_name, resolve_matrix_env, resolve_matrix_values,
    validate_matrix_attrs, validate_task_matrix,
};
pub use memory::{MemoryParseError, parse_memory};
pub use parameters::{
    PARAM_ENV_PREFIX, ParameterError, parameter_env_name, parameter_names,
    parameter_values_from_env, resolve_task_parameter_env, resolve_task_parameter_env_layered,
    resolve_task_parameter_env_with, resolve_task_parameter_values, validate_task_parameters,
};
pub use plan_exec::{
    ArgumentForwarding, EXECUTION_PLAN_SCHEMA_VERSION, ExecutionPlan, FailurePolicy, PlanNode,
    build_execution_plan, build_execution_plan_roots, build_serial_plan,
};
pub use planner::{PlanError, plan_mermaid, plan_serial, plan_serial_union, plan_text};
pub use process::{
    ProcessDefinition, ProcessNameError, ProcessReadiness, ProcessRestart, ReadinessHttp,
    ReadinessTcp, dependency_base_name, parse_processes, sanitize_process_log_name,
    validate_node_id,
};
pub use progress::{
    MAX_PROGRESS_EVENTS_PER_NODE, PROGRESS_EVENT_TYPE, PROGRESS_V1, ProgressState, ProgressUnit,
    ProgressUpdate, StageProgress, StandardUnit, project_progress,
};
pub use receipt::{
    ExecutionReceipt, NOT_ENTERED, RECEIPT_SCHEMA, ReceiptEvent, ShellProbe, ToolObservation,
    attach_events, probe_shell, static_receipt, verified_receipt,
};
pub use remote_execution::{
    ExecutionTransport, cancel_execution, execution_status, replay_execution, submit_execution,
};
pub use remote_journal::{
    RemoteAttempt, begin_remote, cancel_and_bind, observe_replay, record_admitted, record_cancel,
    record_lease, record_replay_cursor, remote_attempt_for_run, remote_run_id, submit_and_bind,
};
pub use remote_receipt::{
    RemoteReceipt, artifact_is_authoritative, dispositions_from_journal, read_remote_receipt,
    store_remote_receipt, verify_artifact,
};
pub use resolve::{
    ResolveTaskError, enrich_apps_with_listing_metadata, listable_tasks, listable_tasks_filtered,
    resolve_task, resolve_task_name,
};
pub use resources::{NodeResources, ResourceLimits};
pub use run_diagnostics::{RunDiagnostic, parse_run_log};
pub use run_events::{RunEventDecorator, format_rfc3339_utc};
pub use run_provenance::{
    ChildObservation, StoredReceipt, child_probe_script, get_artifact, next_attempt,
    parse_child_probe, put_artifact, read_leaf_receipt, read_receipt, transition, write_receipt,
};
pub use scheduler::{NodeState, ScheduleOutcome, Scheduler, SchedulerError};
pub use schema::{
    AppFastPathMetadata, AppListingMetadata, ContextEnvironment, ContextEnvironmentMode,
    ContextSecretRef, EnvInput, EnvInputBinding, ExecutionContext, IoIntensity,
    MAX_SUPPORTED_SCHEMA_VERSION, ParameterConstraints, ParameterPathConstraints,
    ParameterPathKind, SCHEMA_VERSION, SCHEMA_VERSION_V2, SCHEMA_VERSION_V3, SchemaError,
    SecretDelivery, SecretProvider, TaskCache, TaskCacheMode, TaskCacheSecretPolicy,
    TaskDefinition, TaskDocument, TaskInputBinding, TaskInputs, TaskMatrix, TaskOutput,
    TaskOutputMode, TaskParameter, TaskParameterType, TaskResources, WORKING_DIRECTORY_FLAKE_ROOT,
    WORKING_DIRECTORY_INVOCATION, parse_task_document, validate_schema_version,
    validate_working_directory,
};
pub use secrets::{
    ResolvedSecrets, SecureTempFile, authorize_secret_refs, resolve_context_secrets,
    secret_refs_for_entries,
};
pub use selectors::{
    APP_PREFIX, CATEGORY_PREFIX, CHANGED_SELECTOR, ListSelectorResolution, ParsedSelector,
    SelectorError, TASK_PREFIX, TaskTargetResolution, parse_selector, resolve_list_selector,
    resolve_task_targets,
};
pub use validation_plan::{
    PlanCoverage, PlanExecution, PlannedOperation, VALIDATION_PLAN_SCHEMA, ValidationPlan,
    ValidationPlanInput, validation_plan,
};
pub use workflows::{
    WORKFLOWS_RELATIVE_PATH, WORKFLOWS_SCHEMA_VERSION, WorkflowDocument, WorkflowError,
    WorkflowPlanMode, WorkflowPlatform, WorkflowTask, invalidated_by_member,
    load_workflow_document, plan_workflow_document, plan_workflows, workflow_mode_from_selector,
    workflow_path,
};

pub use typed_inputs::{
    INPUT_VALUES_SCHEMA, InputLayers, InputProvenance, InputValuesDocument, ResolvedInput,
    ResolvedInputs, input_environment_value, normalize_parameter_json,
    parameter_contains_secret_reference, parse_input_values, parse_parameter_text,
    resolve_node_inputs, validate_local_paths,
};

pub mod artifact_ref;
pub mod interaction;
pub mod run_view;
