//! Local adapters for durable typed input sessions. No adapter grants NXI authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dialoguer::{Confirm, Input, MultiSelect, Select};
use fs2::FileExt;
use nxr_core::EnvironmentPolicy;
use nxr_nix::OptionalNixFlags;
use nxr_task::catalog::EffectClass;
use nxr_task::interaction::{
    CreateInteraction, DEFAULT_INTERACTION_TTL_SECS, InputAnswer, InputRequest,
    InteractionCompletion, InteractionError, InteractionFrontend, InteractionSession,
    InteractionState, InteractionStore, NewInputRequest, ResumeOutcome,
};
use nxr_task::{
    ExecutionPlan, FailurePolicy, OperationKind, OperationRecord, TaskDocument, TaskParameter,
    TaskParameterType,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::InteractionCommand;
use crate::commands::common::WorkspaceSnapshot;
use crate::commands::session::{LocalBudget, SessionStart};
use crate::commands::task::TaskRequest;
use crate::commands::task_params::{TaskInputOptions, collect_plan_inputs, resolve_plan_inputs};
use crate::output_task::present;
use crate::runner_output::RunnerOutput;

const DESCRIPTOR_SCHEMA: &str = "nxr-interaction-invocation-v1";
const MAX_DESCRIPTOR_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenInvocation {
    schema_version: String,
    workspace_root: PathBuf,
    invocation_directory: PathBuf,
    source_identity: String,
    document_identity: String,
    system: String,
    plan: ExecutionPlan,
    operation_ids: Vec<String>,
    supplied: BTreeMap<String, BTreeMap<String, Value>>,
    caller_parameters: BTreeSet<String>,
    executable: PathBuf,
    executable_digest: String,
    flake: String,
    nix_override: Option<String>,
    root: bool,
    cwd: Option<String>,
    shell: Option<String>,
    shell_mode: String,
    clean_env: bool,
    keep_env: Vec<String>,
    unset_env: Vec<String>,
    jobs: usize,
    keep_going: bool,
    output_mode: Option<String>,
    events_jsonl: bool,
    reports: Vec<String>,
    log_dir: Option<PathBuf>,
    context_override: Option<String>,
    offline: bool,
    accept_flake_config: bool,
    no_write_lock_file: bool,
    /// Effect known when the session was resolved. Absent on older descriptors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effect_class: Option<String>,
}

impl FrozenInvocation {
    fn nix_flags(&self) -> OptionalNixFlags {
        OptionalNixFlags {
            offline: self.offline,
            accept_flake_config: self.accept_flake_config,
            no_write_lock_file: self.no_write_lock_file,
            ..Default::default()
        }
    }
}

/// Existing terminal widgets behind the same request protocol used by MCP.
#[derive(Default)]
pub struct TerminalFrontend;

impl InteractionFrontend for TerminalFrontend {
    fn answer(&mut self, request: &InputRequest) -> Result<Option<Value>, InteractionError> {
        if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
            return Err(InteractionError::Invalid(
                "interactive input requires stdin and stderr TTYs".to_owned(),
            ));
        }
        let prompt = present::format_parameter_prompt(request);
        match request.declaration.param_type {
            TaskParameterType::Choice => prompt_choice(request, &prompt),
            TaskParameterType::Multichoice => prompt_multichoice(request, &prompt),
            TaskParameterType::Boolean => prompt_boolean(request, &prompt),
            _ => prompt_typed_value(request, &prompt),
        }
    }
}

fn prompt_cancelled() -> InteractionError {
    InteractionError::Invalid("terminal input failed or was cancelled".to_owned())
}

fn prompt_choice(request: &InputRequest, prompt: &str) -> Result<Option<Value>, InteractionError> {
    let choices =
        request.declaration.values.as_ref().ok_or_else(|| {
            InteractionError::Invalid("choice declaration has no values".to_owned())
        })?;
    let labels: Vec<_> = choices
        .iter()
        .map(|value| nxr_core::sanitize_terminal_text(value))
        .collect();
    let default = request
        .declaration
        .default
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|selected| choices.iter().position(|value| value == selected))
        .unwrap_or(0);
    let selection = Select::new()
        .with_prompt(prompt)
        .items(&labels)
        .default(default)
        .interact_opt()
        .map_err(|_| prompt_cancelled())?;
    Ok(selection.map(|index| json!(choices[index])))
}

fn prompt_multichoice(
    request: &InputRequest,
    prompt: &str,
) -> Result<Option<Value>, InteractionError> {
    let choices = request.declaration.values.as_ref().ok_or_else(|| {
        InteractionError::Invalid("multichoice declaration has no values".to_owned())
    })?;
    let labels: Vec<_> = choices
        .iter()
        .map(|value| nxr_core::sanitize_terminal_text(value))
        .collect();
    let defaults: Vec<bool> = choices
        .iter()
        .map(|choice| {
            request
                .declaration
                .default
                .as_ref()
                .and_then(Value::as_array)
                .is_some_and(|selected| selected.iter().any(|value| value.as_str() == Some(choice)))
        })
        .collect();
    let selection = MultiSelect::new()
        .with_prompt(prompt)
        .items(&labels)
        .defaults(&defaults)
        .interact_opt()
        .map_err(|_| prompt_cancelled())?;
    Ok(selection.map(|indexes| {
        json!(
            indexes
                .into_iter()
                .map(|index| choices[index].clone())
                .collect::<Vec<_>>()
        )
    }))
}

fn prompt_boolean(request: &InputRequest, prompt: &str) -> Result<Option<Value>, InteractionError> {
    let default = request
        .declaration
        .default
        .as_ref()
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact_opt()
        .map_err(|_| prompt_cancelled())?
        .map(Value::Bool))
}

fn prompt_typed_value(
    request: &InputRequest,
    prompt: &str,
) -> Result<Option<Value>, InteractionError> {
    let required = present::parameter_required(&request.declaration);
    let default_text = non_secret_default_text(&request.declaration);
    loop {
        let mut input = Input::<String>::new().with_prompt(prompt);
        if let Some(default) = &default_text {
            input = input.default(default.clone()).show_default(true);
        }
        input = input.allow_empty(!required || default_text.is_some());
        let raw = match input.interact_text() {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        if raw.is_empty() {
            if let Some(default) = &request.declaration.default
                && !present::is_secret_parameter(&request.declaration)
            {
                return Ok(Some(default.clone()));
            }
            if required {
                eprintln!("value is required");
                continue;
            }
        }
        match nxr_task::parse_parameter_text(
            &request.node_id,
            &request.parameter_name,
            &request.declaration,
            &raw,
        ) {
            Ok(value) => {
                if let Err(error) = nxr_task::normalize_parameter_json(
                    &request.node_id,
                    &request.parameter_name,
                    &request.declaration,
                    &value,
                ) {
                    eprintln!("{error}");
                    continue;
                }
                return Ok(Some(value));
            }
            Err(error) => {
                eprintln!("{error}");
            }
        }
    }
}

fn non_secret_default_text(declaration: &TaskParameter) -> Option<String> {
    if present::is_secret_parameter(declaration) {
        return None;
    }
    declaration.default.as_ref().map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

fn operator_inbox_status(state: InteractionState) -> &'static str {
    match state {
        InteractionState::WaitingForInput
        | InteractionState::CollectingInputs
        | InteractionState::Resolving
        | InteractionState::PlanReady
        | InteractionState::WaitingForAuthorization
        | InteractionState::ReadyToExecute => "waiting",
        InteractionState::Executing => "executing",
        InteractionState::Cancelled => "cancelled",
        InteractionState::Uncertain => "uncertain",
        InteractionState::Completed => "completed",
        InteractionState::Expired => "expired",
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub fn session_directory(root: &Path) -> Result<PathBuf, String> {
    let canonical = root
        .canonicalize()
        .map_err(|_| "interaction workspace is unavailable".to_owned())?;
    let key = nxr_core::digest_bytes(canonical.as_os_str().as_encoded_bytes());
    Ok(nxr_core::state_dir().join("interactions").join(key))
}

fn store_for(root: &Path) -> Result<(InteractionStore, PathBuf, String), String> {
    let directory = session_directory(root)?;
    let store = InteractionStore::new(&directory).map_err(|error| error.to_string())?;
    let directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let authority = local_authority(&directory)?;
    Ok((store, directory, authority))
}

/// Identity comes from a kernel-owned local file, never a tool argument or USER variable.
fn local_authority(directory: &Path) -> Result<String, String> {
    let probe = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "local-uid:{}",
            probe
                .as_file()
                .metadata()
                .map_err(|error| error.to_string())?
                .uid()
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = probe;
        let home = directories::UserDirs::new()
            .ok_or_else(|| "local user profile is unavailable".to_owned())?;
        Ok(format!(
            "local-profile:{}",
            nxr_core::digest_bytes(home.home_dir().as_os_str().as_encoded_bytes())
        ))
    }
}

/// Root task runner hook: called only after a missing-input result and before any spawn.
pub fn create_pending_for_task(
    snapshot: &WorkspaceSnapshot,
    document: &TaskDocument,
    plan: &ExecutionPlan,
    request: &TaskRequest<'_>,
) -> Result<InteractionSession, String> {
    if !plan.interactive_node_ids().collect::<Vec<_>>().is_empty()
        || matches!(
            request.output_mode,
            Some(crate::output_task::TaskOutputMode::Raw | crate::output_task::TaskOutputMode::Tui)
        )
    {
        return Err("resumed execution cannot own a child terminal; resolve inputs and invoke this task in the foreground".to_owned());
    }
    if !request.args.is_empty() {
        return Err("resumable inputs require declared values; forwarded leaf arguments cannot be captured safely".to_owned());
    }
    if !request.nix_flags.nix_options.is_empty()
        || !request.nix_flags.extra_argv.is_empty()
        || request.nix_flags.json_log_format
    {
        return Err(
            "resumable inputs cannot persist arbitrary Nix options; resolve declared inputs first"
                .to_owned(),
        );
    }
    let (clean_env, keep_env, unset_env) = safe_environment_options(&request.environment_policy)?;
    let root = snapshot
        .flake
        .local_root
        .as_ref()
        .ok_or_else(|| "resumable input requires a local workspace".to_owned())?
        .as_std_path();
    let mut options = request.input_options.clone();
    options.interactive = Some(false);
    let partial = collect_plan_inputs(document, plan, &request.param_sets, &options)
        .map_err(|error| error.to_string())?;
    let caller_parameters =
        supplied_parameter_names(document, plan, &request.param_sets, options.file.as_deref())?;
    let invocation = snapshot
        .invocation_directory
        .as_std_path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let executable = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let roots = roots(plan);
    let workspace = nxr_task::workspace_instance_from_checkout(root, "");
    let operation_ids = roots
        .iter()
        .map(|name| {
            let declaration = nxr_task::Declaration::named(OperationKind::Task, name, "flake.nix");
            nxr_task::operation_record(&workspace, &declaration).operation_id
        })
        .collect();
    let descriptor = FrozenInvocation {
        schema_version: DESCRIPTOR_SCHEMA.to_owned(),
        workspace_root: root.canonicalize().map_err(|error| error.to_string())?,
        invocation_directory: invocation,
        source_identity: source_identity(root)?,
        document_identity: json_digest(document)?,
        system: snapshot.nix.system.clone(),
        plan: plan.clone(),
        operation_ids,
        supplied: partial.resolved,
        caller_parameters,
        executable_digest: nxr_core::digest_file(&executable).map_err(|error| error.to_string())?,
        executable,
        flake: snapshot.flake.nix_ref.clone(),
        nix_override: request.nix_override.map(str::to_owned),
        root: request.root,
        cwd: request.cwd.map(str::to_owned),
        shell: request.shell.map(str::to_owned),
        shell_mode: match request.shell_mode {
            crate::shell_mode::ShellMode::Smart => "smart",
            crate::shell_mode::ShellMode::Always => "always",
            crate::shell_mode::ShellMode::Never => "never",
        }
        .to_owned(),
        clean_env,
        keep_env,
        unset_env,
        jobs: request.jobs,
        keep_going: request.keep_going,
        output_mode: request.output_mode.map(|mode| {
            match mode {
                crate::output_task::TaskOutputMode::Compact => "compact",
                crate::output_task::TaskOutputMode::Live => "live",
                crate::output_task::TaskOutputMode::Grouped => "grouped",
                crate::output_task::TaskOutputMode::Failures => "failures",
                crate::output_task::TaskOutputMode::Summary => "summary",
                crate::output_task::TaskOutputMode::Tui => "tui",
                crate::output_task::TaskOutputMode::Raw => "raw",
            }
            .to_owned()
        }),
        events_jsonl: request.events_format.is_some(),
        reports: [
            ("junit", &request.reports.junit),
            ("sarif", &request.reports.sarif),
            ("coverage", &request.reports.coverage),
            ("benchmark", &request.reports.benchmark),
        ]
        .into_iter()
        .filter_map(|(kind, path)| {
            path.as_ref()
                .map(|path| format!("{kind}={}", path.display()))
        })
        .collect(),
        log_dir: request.log_dir.clone(),
        context_override: request.context_override.clone(),
        offline: request.nix_flags.offline,
        accept_flake_config: request.nix_flags.accept_flake_config,
        no_write_lock_file: request.nix_flags.no_write_lock_file,
        effect_class: roots
            .first()
            .map(|name| EffectClass::from_declaration(OperationKind::Task, name).to_string()),
    };
    create_frozen(descriptor, partial.missing, None)
}

fn safe_environment_options(
    policy: &EnvironmentPolicy,
) -> Result<(bool, Vec<String>, Vec<String>), String> {
    match policy {
        EnvironmentPolicy::Inherit => Ok((false, Vec::new(), Vec::new())),
        EnvironmentPolicy::InheritWith { keep, set, unset }
        | EnvironmentPolicy::Clean { keep, set, unset } => {
            if !set.is_empty() {
                return Err("resumable inputs cannot persist raw environment values; declare typed inputs or secret references".to_owned());
            }
            Ok((
                matches!(policy, EnvironmentPolicy::Clean { .. }),
                keep.clone(),
                unset.clone(),
            ))
        }
    }
}

fn supplied_parameter_names(
    document: &TaskDocument,
    plan: &ExecutionPlan,
    sets: &BTreeMap<String, String>,
    file: Option<&Path>,
) -> Result<BTreeSet<String>, String> {
    let expanded =
        nxr_task::expand_matrix_tasks(&document.tasks).map_err(|error| error.to_string())?;
    let mut names = BTreeSet::new();
    for node in &plan.nodes {
        if let Some(task) = expanded
            .tasks
            .get(&node.id)
            .or_else(|| document.tasks.get(&node.id))
        {
            for name in task.parameters.keys() {
                if sets.contains_key(name) || sets.contains_key(&format!("{}.{name}", node.id)) {
                    names.insert(format!("{}.{name}", node.id));
                }
            }
        }
    }
    if let Some(file) = file {
        let value: Value = serde_json::from_slice(&read_bounded(file)?)
            .map_err(|_| "invalid input values document".to_owned())?;
        for (node, parameters) in nxr_task::parse_input_values(&value)
            .map_err(|error| error.to_string())?
            .inputs
        {
            names.extend(parameters.keys().map(|name| format!("{node}.{name}")));
        }
    }
    Ok(names)
}

fn create_frozen(
    descriptor: FrozenInvocation,
    missing: Vec<(String, String, nxr_task::TaskParameter)>,
    key: Option<&str>,
) -> Result<InteractionSession, String> {
    let (store, directory, authority) = store_for(&descriptor.workspace_root)?;
    let fingerprint = json_digest(&descriptor)?;
    let create = || {
        let requests = missing
            .into_iter()
            .map(|(node, parameter, declaration)| NewInputRequest {
                request_id: format!(
                    "input-{}",
                    nxr_core::digest_bytes(format!("{node}\n{parameter}").as_bytes())
                ),
                node_id: node,
                parameter_name: parameter,
                declaration,
                observation_digest: None,
            })
            .collect();
        let session = store
            .create(CreateInteraction {
                operation_id: descriptor
                    .operation_ids
                    .first()
                    .cloned()
                    .ok_or_else(|| "interaction has no root operation".to_owned())?,
                caller_authority: authority.clone(),
                requests,
                now: now_secs(),
                ttl_secs: DEFAULT_INTERACTION_TTL_SECS,
            })
            .map_err(|error| error.to_string())?;
        write_private(
            &directory.join(format!("{}.invocation.json", session.session_id)),
            &descriptor,
        )?;
        let session = if session.pending_requests.is_empty() {
            let digest = resolved_plan_digest(&descriptor, &descriptor.supplied)?;
            store
                .ready(
                    &session.session_id,
                    session.revision,
                    &authority,
                    &digest,
                    now_secs(),
                )
                .map_err(|error| error.to_string())?
        } else {
            session
        };
        Ok::<_, String>(session)
    };
    let Some(key) = key else {
        return create();
    };
    if key.is_empty() || key.len() > 1024 {
        return Err("idempotency key is empty or too long".to_owned());
    }
    let key_digest = nxr_core::digest_bytes(
        format!("{}\n{key}", descriptor.operation_ids.join("\n")).as_bytes(),
    );
    let lock_path = directory.join(format!("key-{key_digest}.lock"));
    ensure_regular_or_absent(&lock_path)?;
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(lock_path).map_err(|error| error.to_string())?;
    FileExt::lock_exclusive(&lock).map_err(|error| error.to_string())?;
    let path = directory.join(format!("key-{key_digest}.json"));
    if path.exists() {
        let existing: Value = serde_json::from_slice(&read_bounded(&path)?)
            .map_err(|_| "interaction key record is corrupt".to_owned())?;
        if existing.get("fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str()) {
            return Err("idempotency key conflicts with the frozen invocation".to_owned());
        }
        let id = existing
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "interaction key has no session".to_owned())?;
        return store
            .get(id, &authority, now_secs())
            .map_err(|error| error.to_string());
    }
    let session = create()?;
    write_private(
        &path,
        &json!({"session_id":session.session_id,"fingerprint":fingerprint}),
    )?;
    Ok(session)
}

pub fn input_required(session: &InteractionSession) -> Value {
    let status = match session.state {
        InteractionState::WaitingForInput | InteractionState::CollectingInputs => "input_required",
        InteractionState::Resolving => "resolving",
        InteractionState::PlanReady => "plan_ready",
        InteractionState::WaitingForAuthorization => "waiting_for_authorization",
        InteractionState::ReadyToExecute => "ready_to_execute",
        InteractionState::Executing => "executing",
        InteractionState::Completed => "completed",
        InteractionState::Cancelled => "cancelled",
        InteractionState::Expired => "expired",
        InteractionState::Uncertain => "uncertain",
    };
    json!({"schema_version":"nxr-interaction-v1","status":status,"session":session})
}

pub fn inspect_session(root: &Path, session_id: &str) -> Result<Value, String> {
    let (store, _, authority) = store_for(root)?;
    let session = store
        .get(session_id, &authority, now_secs())
        .map_err(|error| error.to_string())?;
    if let Some(lease) = &session.execution_lease
        && matches!(
            session.state,
            InteractionState::Executing | InteractionState::Uncertain
        )
    {
        return existing_execution(
            &store,
            &session,
            lease,
            &authority,
            &nxr_core::daemon_socket_path(),
        );
    }
    Ok(input_required(&session))
}

/// List interaction sessions for the workspace with operator-facing status.
pub fn inbox_sessions(root: &Path) -> Result<Value, String> {
    let (store, _, authority) = store_for(root)?;
    let sessions = store
        .list(&authority, now_secs())
        .map_err(|error| error.to_string())?;
    let entries: Vec<_> = sessions
        .iter()
        .map(|session| {
            json!({
                "session_id": session.session_id,
                "operation_id": session.operation_id,
                "revision": session.revision,
                "state": session.state,
                "status": operator_inbox_status(session.state),
                "pending_requests": session.pending_requests.len(),
                "execution_lease": session.execution_lease,
            })
        })
        .collect();
    Ok(json!({
        "schema_version": "nxr-interaction-inbox-v1",
        "sessions": entries,
    }))
}

/// Interactively answer outstanding requests and optionally edit values before execute.
///
/// Cancelling one prompt leaves the session (and every other session) intact.
pub fn prompt_session(
    root: &Path,
    session_id: &str,
    revision: Option<u64>,
) -> Result<Value, String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err("interactive prompt requires stdin and stderr TTYs".to_owned());
    }
    let (store, _, authority) = store_for(root)?;
    let mut session = store
        .get(session_id, &authority, now_secs())
        .map_err(|error| error.to_string())?;
    if let Some(expected) = revision
        && session.revision != expected
    {
        return Err(format!(
            "interaction revision is stale (expected {}, got {expected})",
            session.revision
        ));
    }
    let mut frontend = TerminalFrontend;
    while !session.pending_requests.is_empty() {
        if session.state != InteractionState::WaitingForInput {
            break;
        }
        let request = session.pending_requests[0].clone();
        let Some(value) = frontend
            .answer(&request)
            .map_err(|error| error.to_string())?
        else {
            // One cancelled prompt must not delete this or any other session.
            return Ok(json!({
                "schema_version": "nxr-interaction-v1",
                "status": "prompt_cancelled",
                "session": session,
            }));
        };
        let result = store
            .answer(
                InputAnswer {
                    session_id: session.session_id.clone(),
                    request_id: request.request_id.clone(),
                    revision: session.revision,
                    caller_authority: authority.clone(),
                    value,
                    observation_digest: request.observation_digest.clone(),
                },
                &authority,
                now_secs(),
            )
            .map_err(|error| error.to_string())?;
        session = result.session;
    }
    session = review_and_edit_answers(&store, &mut frontend, session, &authority)?;
    Ok(input_required(&session))
}

fn review_and_edit_answers(
    store: &InteractionStore,
    frontend: &mut TerminalFrontend,
    mut session: InteractionSession,
    authority: &str,
) -> Result<InteractionSession, String> {
    if session.accepted_answers.is_empty()
        || !matches!(
            session.state,
            InteractionState::Resolving
                | InteractionState::PlanReady
                | InteractionState::WaitingForAuthorization
                | InteractionState::ReadyToExecute
                | InteractionState::WaitingForInput
        )
        || session.execution_lease.is_some()
    {
        return Ok(session);
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Ok(session);
    }
    loop {
        let mut entries: Vec<AcceptedEditable> = session
            .accepted_answers
            .values()
            .map(|accepted| AcceptedEditable {
                request_id: accepted.request.request_id.clone(),
                node_id: accepted.request.node_id.clone(),
                parameter_name: accepted.request.parameter_name.clone(),
                declaration: accepted.request.declaration.clone(),
                observation_digest: accepted.request.observation_digest.clone(),
                value: accepted.value.clone(),
            })
            .collect();
        entries.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.parameter_name.cmp(&right.parameter_name))
        });
        let mut labels = vec!["done — keep current values".to_owned()];
        for entry in &entries {
            let preview = if present::is_secret_parameter(&entry.declaration) {
                "(hidden)".to_owned()
            } else {
                nxr_core::sanitize_terminal_text(&entry.value.to_string())
            };
            labels.push(format!(
                "edit {}.{} = {preview}",
                entry.node_id, entry.parameter_name
            ));
        }
        let selection = Select::new()
            .with_prompt("Review answers before execute")
            .items(&labels)
            .default(0)
            .interact_opt()
            .map_err(|error| error.to_string())?;
        let Some(index) = selection else {
            return Ok(session);
        };
        if index == 0 {
            return Ok(session);
        }
        let entry = entries[index - 1].clone();
        session = store
            .refresh_request(
                &session.session_id,
                session.revision,
                authority,
                NewInputRequest {
                    request_id: entry.request_id.clone(),
                    node_id: entry.node_id.clone(),
                    parameter_name: entry.parameter_name.clone(),
                    declaration: entry.declaration.clone(),
                    observation_digest: entry.observation_digest.clone(),
                },
                now_secs(),
            )
            .map_err(|error| error.to_string())?;
        let request = session
            .pending_requests
            .iter()
            .find(|request| request.request_id == entry.request_id)
            .cloned()
            .ok_or_else(|| "edited request disappeared".to_owned())?;
        let Some(value) = frontend
            .answer(&request)
            .map_err(|error| error.to_string())?
        else {
            return Ok(session);
        };
        session = store
            .answer(
                InputAnswer {
                    session_id: session.session_id.clone(),
                    request_id: request.request_id,
                    revision: session.revision,
                    caller_authority: authority.to_owned(),
                    value,
                    observation_digest: request.observation_digest,
                },
                authority,
                now_secs(),
            )
            .map_err(|error| error.to_string())?
            .session;
    }
}

#[derive(Clone, Debug)]
struct AcceptedEditable {
    request_id: String,
    node_id: String,
    parameter_name: String,
    declaration: TaskParameter,
    observation_digest: Option<String>,
    value: Value,
}

pub fn answer_session(
    root: &Path,
    session_id: &str,
    request_id: &str,
    revision: u64,
    value: Value,
    observation_digest: Option<String>,
) -> Result<Value, String> {
    let (store, _, authority) = store_for(root)?;
    let result = store
        .answer(
            InputAnswer {
                session_id: session_id.to_owned(),
                request_id: request_id.to_owned(),
                revision,
                caller_authority: authority.clone(),
                value,
                observation_digest,
            },
            &authority,
            now_secs(),
        )
        .map_err(|error| error.to_string())?;
    let mut value = input_required(&result.session);
    value["repeated"] = json!(result.repeated);
    Ok(value)
}

pub fn cancel_session(
    root: &Path,
    session_id: &str,
    revision: u64,
    socket: &Path,
) -> Result<Value, String> {
    let (store, _, authority) = store_for(root)?;
    let session = store
        .get(session_id, &authority, now_secs())
        .map_err(|error| error.to_string())?;
    if session.revision != revision && session.state != InteractionState::Cancelled {
        return Err(format!(
            "interaction revision is stale (expected {}, got {revision})",
            session.revision
        ));
    }
    if let Some(lease) = &session.execution_lease {
        let run_id = lease.supervisor_run_id.as_deref().ok_or_else(|| {
            "execution launch is uncertain; inspect before cancellation".to_owned()
        })?;
        let result = crate::commands::session::cancel_on(socket, run_id)?;
        let status = result.get("status").and_then(Value::as_str).unwrap_or("");
        if status != "cancelled" {
            return Ok(
                json!({"schema_version":"nxr-interaction-v1","session":session,"cancel_requested":true,"started":false,"run":result}),
            );
        }
        let result = store
            .finish(
                session_id,
                &lease.lease_id,
                &authority,
                InteractionCompletion::Cancelled,
            )
            .map_err(|error| error.to_string())?;
        return Ok(input_required(&result));
    }
    let result = store
        .cancel(session_id, revision, &authority, now_secs())
        .map_err(|error| error.to_string())?;
    Ok(input_required(&result))
}

/// Discover declarations without collecting values or requiring execution permission.
pub fn operation_inputs(
    root: &Path,
    record: &OperationRecord,
    nix_override: Option<&str>,
    flags: &OptionalNixFlags,
) -> Result<Value, String> {
    let snapshot = WorkspaceSnapshot::load(
        Some(root.to_string_lossy().as_ref()),
        nix_override,
        true,
        flags,
    )
    .map_err(|error| error.to_string())?;
    let document = snapshot
        .tasks
        .as_ref()
        .ok_or_else(|| "workspace has no task document".to_owned())?;
    let plan = operation_plan(document, record)?;
    let expanded =
        nxr_task::expand_matrix_tasks(&document.tasks).map_err(|error| error.to_string())?;
    let inputs: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .filter_map(|node| {
            expanded
                .tasks
                .get(&node.id)
                .map(|task| (node.id.clone(), task.parameters.clone()))
        })
        .collect();
    Ok(
        json!({"schema_version":"nxr-input-declarations-v1","operation_id":record.operation_id,"execution_allowed":record.execution_allowed,"inputs":inputs}),
    )
}

pub fn operation_has_inputs(
    root: &Path,
    record: &OperationRecord,
    nix_override: Option<&str>,
    flags: &OptionalNixFlags,
) -> Result<bool, String> {
    if record.kind != OperationKind::Task {
        return Ok(false);
    }
    let value = operation_inputs(root, record, nix_override, flags)?;
    Ok(value
        .get("inputs")
        .and_then(Value::as_object)
        .is_some_and(|nodes| {
            nodes.values().any(|parameters| {
                parameters
                    .as_object()
                    .is_some_and(|parameters| !parameters.is_empty())
            })
        }))
}

/// Resolve static inputs into one durable session; this never starts execution.
pub fn resolve_operation(
    root: &Path,
    record: &OperationRecord,
    inputs: Option<&Value>,
    sets: &BTreeMap<String, String>,
    key: Option<&str>,
    nix_override: Option<&str>,
    flags: &OptionalNixFlags,
) -> Result<InteractionSession, String> {
    let snapshot = WorkspaceSnapshot::load(
        Some(root.to_string_lossy().as_ref()),
        nix_override,
        true,
        flags,
    )
    .map_err(|error| error.to_string())?;
    let document = snapshot
        .tasks
        .as_ref()
        .ok_or_else(|| "workspace has no task document".to_owned())?;
    let plan = operation_plan(document, record)?;
    let (clean_env, keep_env, unset_env) = (false, Vec::new(), Vec::new());
    if !flags.nix_options.is_empty() || !flags.extra_argv.is_empty() {
        return Err("resumable input cannot persist arbitrary Nix options".to_owned());
    }
    let (temporary, file) = input_argument_file(inputs, document, &plan, sets)?;
    let options = TaskInputOptions {
        file,
        interactive: Some(false),
        context_override: None,
        invocation_directory: Some(snapshot.invocation_directory.as_std_path().to_owned()),
    };
    let partial =
        collect_plan_inputs(document, &plan, sets, &options).map_err(|error| error.to_string())?;
    let caller_parameters =
        supplied_parameter_names(document, &plan, sets, options.file.as_deref())?;
    drop(temporary);
    let executable = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let descriptor = FrozenInvocation {
        schema_version: DESCRIPTOR_SCHEMA.to_owned(),
        workspace_root: root.canonicalize().map_err(|error| error.to_string())?,
        invocation_directory: snapshot
            .invocation_directory
            .as_std_path()
            .canonicalize()
            .map_err(|error| error.to_string())?,
        source_identity: source_identity(root)?,
        document_identity: json_digest(document)?,
        system: snapshot.nix.system.clone(),
        plan,
        operation_ids: vec![record.operation_id.clone()],
        supplied: partial.resolved,
        caller_parameters,
        executable_digest: nxr_core::digest_file(&executable).map_err(|error| error.to_string())?,
        executable,
        flake: snapshot.flake.nix_ref.clone(),
        nix_override: nix_override.map(str::to_owned),
        root: false,
        cwd: None,
        shell: None,
        shell_mode: "smart".to_owned(),
        clean_env,
        keep_env,
        unset_env,
        jobs: 1,
        keep_going: false,
        output_mode: None,
        events_jsonl: false,
        reports: Vec::new(),
        log_dir: None,
        context_override: None,
        offline: flags.offline,
        accept_flake_config: flags.accept_flake_config,
        no_write_lock_file: flags.no_write_lock_file,
        effect_class: Some(
            record
                .authorized_effect
                .unwrap_or(record.inferred_effect)
                .to_string(),
        ),
    };
    create_frozen(descriptor, partial.missing, key)
}

fn operation_plan(
    document: &TaskDocument,
    record: &OperationRecord,
) -> Result<ExecutionPlan, String> {
    if record.kind != OperationKind::Task {
        return Err("typed interaction currently requires a task operation".to_owned());
    }
    let canonical =
        nxr_task::resolve_task_name(document, &record.name).map_err(|error| error.to_string())?;
    nxr_task::build_execution_plan(&document.tasks, canonical, FailurePolicy::FailFast, None)
        .map_err(|error| error.to_string())
}

fn input_argument_file(
    inputs: Option<&Value>,
    document: &TaskDocument,
    plan: &ExecutionPlan,
    sets: &BTreeMap<String, String>,
) -> Result<(Option<tempfile::NamedTempFile>, Option<PathBuf>), String> {
    let Some(inputs) = inputs else {
        return Ok((None, None));
    };
    let value = if inputs.get("schema_version").is_some() {
        inputs.clone()
    } else {
        json!({"schema_version":nxr_task::INPUT_VALUES_SCHEMA,"inputs":inputs})
    };
    let mut parsed = nxr_task::parse_input_values(&value).map_err(|error| error.to_string())?;
    let expansion =
        nxr_task::expand_matrix_tasks(&document.tasks).map_err(|error| error.to_string())?;
    let nodes: BTreeSet<_> = plan.nodes.iter().map(|node| node.id.as_str()).collect();
    for (node, values) in &mut parsed.inputs {
        if !nodes.contains(node.as_str()) {
            return Err("input document names a node outside the plan".to_owned());
        }
        let task = expansion.tasks.get(node).ok_or("unknown input node")?;
        for (name, value) in values.iter_mut() {
            let declaration = task.parameters.get(name).ok_or("unknown input parameter")?;
            // A higher priority CLI value replaces, rather than serializes, a
            // discarded lower priority value.
            if let Some(raw) = sets
                .get(&format!("{node}.{name}"))
                .or_else(|| sets.get(name))
            {
                *value = nxr_task::parse_parameter_text(node, name, declaration, raw)
                    .map_err(|error| error.to_string())?;
            }
            *value = nxr_task::normalize_parameter_json(node, name, declaration, value)
                .map_err(|error| error.to_string())?;
        }
    }
    let value = serde_json::to_value(parsed).map_err(|error| error.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut temporary, &value).map_err(|error| error.to_string())?;
    let path = temporary.path().to_owned();
    Ok((Some(temporary), Some(path)))
}

pub fn resume_session(
    root: &Path,
    session_id: &str,
    revision: u64,
    socket: &Path,
    runner: RunnerOutput,
) -> Result<Value, String> {
    let (store, directory, authority) = store_for(root)?;
    let mut session = store
        .get(session_id, &authority, now_secs())
        .map_err(|error| error.to_string())?;
    let descriptor = read_descriptor(&directory, session_id)?;
    verify_scope(root, &descriptor)?;
    authorize_execution(&descriptor, &session)?;
    if let Some(lease) = &session.execution_lease {
        return existing_execution(&store, &session, lease, &authority, socket);
    }
    if session.revision != revision {
        return Err(format!(
            "interaction revision is stale (expected {}, got {revision})",
            session.revision
        ));
    }
    if !session.pending_requests.is_empty() {
        return Ok(input_required(&session));
    }
    let flags = descriptor.nix_flags();
    let snapshot = WorkspaceSnapshot::load_with_refresh(
        Some(&descriptor.flake),
        descriptor.nix_override.as_deref(),
        true,
        &flags,
        true,
    )
    .map_err(|error| error.to_string())?;
    let document = snapshot
        .tasks
        .as_ref()
        .ok_or_else(|| "workspace has no task document".to_owned())?;
    if snapshot.nix.system != descriptor.system
        || json_digest(document)? != descriptor.document_identity
        || source_identity(root)? != descriptor.source_identity
    {
        return Err(
            "interaction source or task declarations changed; resolve a new session".to_owned(),
        );
    }
    if nxr_core::digest_file(&descriptor.executable).map_err(|error| error.to_string())?
        != descriptor.executable_digest
    {
        return Err("interaction runner changed; resolve a new session".to_owned());
    }
    let values = merged_values(&descriptor, &session);
    let input_path = directory.join(format!("{session_id}.values.json"));
    write_or_verify_private(
        &input_path,
        &json!({"schema_version":nxr_task::INPUT_VALUES_SCHEMA,"inputs":values}),
    )?;
    let options = TaskInputOptions {
        file: Some(input_path.clone()),
        interactive: Some(false),
        context_override: descriptor.context_override.clone(),
        invocation_directory: Some(descriptor.invocation_directory.clone()),
    };
    let resolved = resolve_plan_inputs(document, &descriptor.plan, &BTreeMap::new(), &options)
        .map_err(|error| error.to_string())?;
    let normalized: BTreeMap<_, _> = resolved
        .into_iter()
        .map(|(node, values)| {
            (
                node,
                values
                    .values
                    .into_iter()
                    .map(|(name, value)| (name, value.value))
                    .collect::<BTreeMap<_, _>>(),
            )
        })
        .collect();
    let digest = resolved_plan_digest(&descriptor, &normalized)?;
    print_effect_review(&descriptor, document, &session, &normalized);
    if !matches!(session.state, InteractionState::ReadyToExecute) {
        session = store
            .ready(
                session_id,
                session.revision,
                &authority,
                &digest,
                now_secs(),
            )
            .map_err(|error| error.to_string())?;
    }
    let run_id = format!(
        "run-interaction-{}",
        &nxr_core::digest_bytes(session_id.as_bytes())[..24]
    );
    let command = command_for_inputs(&descriptor, &input_path)?;
    let request = SessionStart {
        label: format!("interaction:{session_id}"),
        command,
        remote: None,
        attempt_id: None,
        cpu: 0,
        memory_mib: 0,
        cwd: Some(descriptor.invocation_directory.clone()),
        admission_id: Some(format!("interaction:{session_id}")),
        target_dir: None,
        operation_id: Some(session.operation_id.clone()),
        workspace_id: Some(nxr_workspace_id(root)),
        output_dirs: Vec::new(),
        probe_script: None,
        run_id: Some(run_id.clone()),
        source_digest: Some(digest.clone()),
        leaf_id: Some("task-dag".to_owned()),
        qualify: false,
        parameters: None,
    };
    dispatch_reserved(&store, &session, &authority, &digest, &run_id, || {
        crate::commands::session::start_on(
            socket,
            &request,
            LocalBudget::from_process(),
            &nxr_task::compiler_cache::compiler_cache_from_process(),
            runner,
        )
    })
}

fn print_effect_review(
    descriptor: &FrozenInvocation,
    document: &TaskDocument,
    session: &nxr_task::interaction::InteractionSession,
    normalized: &BTreeMap<String, BTreeMap<String, Value>>,
) {
    let Some(raw) = descriptor.effect_class.as_deref() else {
        return;
    };
    let Ok(effect) = serde_json::from_value(Value::String(raw.to_owned())) else {
        return;
    };
    if !present::effect_requires_prelaunch_review(effect) {
        return;
    }
    let Ok(expanded) = nxr_task::expand_matrix_tasks(&document.tasks) else {
        return;
    };
    let mut answers = BTreeMap::new();
    for (node, values) in normalized {
        let Some(task) = expanded
            .tasks
            .get(node)
            .or_else(|| document.tasks.get(node))
        else {
            continue;
        };
        for (name, value) in values {
            let Some(declaration) = task.parameters.get(name) else {
                continue;
            };
            answers
                .entry(node.clone())
                .or_insert_with(BTreeMap::new)
                .insert(name.clone(), (declaration.clone(), value.clone()));
        }
    }
    for line in present::format_prelaunch_review_lines(&session.operation_id, effect, &answers) {
        eprintln!("{line}");
    }
}

fn nxr_workspace_id(root: &Path) -> String {
    nxr_task::workspace_instance_from_checkout(root, "").workspace_id
}

fn dispatch_reserved(
    store: &InteractionStore,
    session: &InteractionSession,
    authority: &str,
    digest: &str,
    run_id: &str,
    launch: impl FnOnce() -> Result<String, String>,
) -> Result<Value, String> {
    match store
        .resume(
            &session.session_id,
            session.revision,
            authority,
            digest,
            now_secs(),
        )
        .map_err(|error| error.to_string())?
    {
        ResumeOutcome::Existing { session } => {
            Ok(json!({"session":session,"started":false,"replayed":true}))
        }
        ResumeOutcome::Reserved { session, lease_id } => {
            // The supervisor identity is durable before launch. An acknowledgement
            // loss can only reconcile this exact ID, never reserve another one.
            let bound = store
                .bind_execution(&session.session_id, &lease_id, authority, run_id)
                .map_err(|error| error.to_string())?;
            match launch() {
                Ok(returned) if returned == run_id => Ok(
                    json!({"schema_version":"nxr-interaction-v1","session":bound,"run_id":run_id,"started":true,"replayed":false}),
                ),
                Ok(_) | Err(_) => {
                    let uncertain = store
                        .mark_uncertain(&session.session_id, &lease_id, authority)
                        .map_err(|error| error.to_string())?;
                    Ok(
                        json!({"schema_version":"nxr-interaction-v1","session":uncertain,"run_id":run_id,"started":false,"replayed":false,"status":"uncertain","message":"supervisor launch acknowledgement unavailable; inspect this run before any further action"}),
                    )
                }
            }
        }
    }
}

fn existing_execution(
    store: &InteractionStore,
    session: &InteractionSession,
    lease: &nxr_task::interaction::ExecutionLease,
    authority: &str,
    socket: &Path,
) -> Result<Value, String> {
    let Some(run_id) = &lease.supervisor_run_id else {
        return Ok(json!({"session":session,"started":false,"replayed":true,"status":"uncertain"}));
    };
    let observed = crate::commands::session::inspect_on(socket, run_id);
    let result = match observed {
        Ok(value) => {
            let status = value.get("status").and_then(Value::as_str).unwrap_or("");
            let completion = match status {
                "succeeded" => Some(InteractionCompletion::Succeeded),
                "failed" => Some(InteractionCompletion::Failed),
                "exited" => value.get("exit_code").and_then(Value::as_i64).map(|code| {
                    if code == 0 {
                        InteractionCompletion::Succeeded
                    } else {
                        InteractionCompletion::Failed
                    }
                }),
                "cancelled" => Some(InteractionCompletion::Cancelled),
                _ => None,
            };
            let current = if let Some(completion) = completion {
                store
                    .finish(&session.session_id, &lease.lease_id, authority, completion)
                    .map_err(|error| error.to_string())?
            } else {
                session.clone()
            };
            json!({"schema_version":"nxr-interaction-v1","session":current,"run_id":run_id,"started":false,"replayed":true,"run":value})
        }
        Err(_) => {
            let current = if matches!(
                session.state,
                InteractionState::Executing | InteractionState::Uncertain
            ) {
                store
                    .mark_uncertain(&session.session_id, &lease.lease_id, authority)
                    .map_err(|error| error.to_string())?
            } else {
                session.clone()
            };
            json!({"schema_version":"nxr-interaction-v1","session":current,"run_id":run_id,"started":false,"replayed":true,"status":"uncertain"})
        }
    };
    Ok(result)
}

fn authorize_execution(
    descriptor: &FrozenInvocation,
    session: &InteractionSession,
) -> Result<(), String> {
    if !crate::commands::mcp::workspace_trusted(&descriptor.workspace_root) {
        return Err("workspace is not trusted".to_owned());
    }
    let flags = descriptor.nix_flags();
    let records = crate::commands::op::load_records(
        Some(&descriptor.flake),
        descriptor.nix_override.as_deref(),
        &flags,
        quiet_runner(),
    )?;
    let visible = crate::commands::mcp::policy_view(&records, &descriptor.workspace_root, true)?;
    let gate = crate::commands::mcp::policy_gate(&descriptor.workspace_root)?;
    let root_names = roots(&descriptor.plan);
    let mut caller = descriptor.caller_parameters.clone();
    for accepted in session.accepted_answers.values() {
        caller.insert(format!(
            "{}.{}",
            accepted.request.node_id, accepted.request.parameter_name
        ));
    }
    for (index, id) in descriptor.operation_ids.iter().enumerate() {
        let record = nxr_task::describe(&visible, id)
            .ok_or_else(|| "interaction operation disappeared".to_owned())?;
        if !record.execution_allowed
            || Path::new(&record.workspace_root)
                .canonicalize()
                .map_err(|error| error.to_string())?
                != descriptor.workspace_root
        {
            return Err("operation is not executable in the bound workspace".to_owned());
        }
        let root_name = root_names
            .get(index)
            .ok_or_else(|| "interaction root binding is invalid".to_owned())?;
        let names = caller_policy_names(&descriptor.plan, root_name, &caller);
        nxr_task::validate_policy_parameters(&gate.applied, id, &names)?;
    }
    Ok(())
}

fn caller_policy_names(
    plan: &ExecutionPlan,
    root: &str,
    caller: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut pending = vec![root.to_owned()];
    let mut reachable = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !reachable.insert(node.clone()) {
            continue;
        }
        if let Some(definition) = plan.nodes.iter().find(|definition| definition.id == node) {
            pending.extend(definition.depends_on.iter().cloned());
        }
    }
    caller
        .iter()
        .filter_map(|name| {
            let node = reachable
                .iter()
                .filter(|node| {
                    name.strip_prefix(&format!("{node}."))
                        .is_some_and(|suffix| !suffix.contains('.'))
                })
                .max_by_key(|node| node.len())?;
            let parameter = name.strip_prefix(&format!("{node}."))?;
            Some((
                if node == root {
                    parameter.to_owned()
                } else {
                    name.clone()
                },
                String::new(),
            ))
        })
        .collect()
}

fn merged_values(
    descriptor: &FrozenInvocation,
    session: &InteractionSession,
) -> BTreeMap<String, BTreeMap<String, Value>> {
    let mut values = descriptor.supplied.clone();
    for (node, answers) in session.answer_values() {
        values.entry(node).or_default().extend(answers);
    }
    values
}

fn resolved_plan_digest(
    descriptor: &FrozenInvocation,
    values: &BTreeMap<String, BTreeMap<String, Value>>,
) -> Result<String, String> {
    json_digest(&json!({"invocation":descriptor,"resolved_inputs":values}))
}

fn command_for_inputs(
    descriptor: &FrozenInvocation,
    input_path: &Path,
) -> Result<Vec<String>, String> {
    let mut command = vec![
        descriptor.executable.display().to_string(),
        "--flake".to_owned(),
        descriptor.flake.clone(),
    ];
    if let Some(nix) = &descriptor.nix_override {
        command.extend(["--nix".to_owned(), nix.clone()]);
    }
    if descriptor.root {
        command.push("--root".to_owned());
    }
    if let Some(cwd) = &descriptor.cwd {
        command.extend(["--cwd".to_owned(), cwd.clone()]);
    }
    if let Some(shell) = &descriptor.shell {
        command.extend(["--shell".to_owned(), shell.clone()]);
    }
    command.extend(["--shell-mode".to_owned(), descriptor.shell_mode.clone()]);
    if descriptor.clean_env {
        command.push("--clean-env".to_owned());
    }
    for name in &descriptor.keep_env {
        command.extend(["--keep-env".to_owned(), name.clone()]);
    }
    for name in &descriptor.unset_env {
        command.extend(["--unset-env".to_owned(), name.clone()]);
    }
    if let Some(mode) = &descriptor.output_mode {
        command.extend(["--output".to_owned(), mode.clone()]);
    }
    if descriptor.events_jsonl {
        command.extend(["--events".to_owned(), "jsonl".to_owned()]);
    }
    if let Some(path) = &descriptor.log_dir {
        command.extend(["--log-dir".to_owned(), path.display().to_string()]);
    }
    for report in &descriptor.reports {
        command.extend(["--report".to_owned(), report.clone()]);
    }
    if descriptor.offline {
        command.push("--offline".to_owned());
    }
    if descriptor.accept_flake_config {
        command.push("--accept-flake-config".to_owned());
    }
    if descriptor.no_write_lock_file {
        command.push("--nix-arg=--no-write-lock-file".to_owned());
    }
    if let Some(context) = &descriptor.context_override {
        command.extend(["--context".to_owned(), context.clone()]);
    }
    command.extend([
        "task".to_owned(),
        "--non-interactive".to_owned(),
        "--inputs".to_owned(),
        input_path.display().to_string(),
        "--jobs".to_owned(),
        descriptor.jobs.to_string(),
    ]);
    if descriptor.keep_going {
        command.push("--keep-going".to_owned());
    }
    command.extend(roots(&descriptor.plan));
    Ok(command)
}

fn verify_scope(root: &Path, descriptor: &FrozenInvocation) -> Result<(), String> {
    if descriptor.schema_version != DESCRIPTOR_SCHEMA
        || root.canonicalize().map_err(|error| error.to_string())? != descriptor.workspace_root
        || !descriptor.invocation_directory.is_absolute()
        || !descriptor.invocation_directory.is_dir()
        || descriptor.jobs == 0
        || descriptor.operation_ids.len() != roots(&descriptor.plan).len()
    {
        return Err("frozen interaction scope is invalid".to_owned());
    }
    Ok(())
}

fn source_identity(root: &Path) -> Result<String, String> {
    match nxr_task::source_set_from_repo(root) {
        Ok(source) => Ok(nxr_task::source_set_digest(&source)),
        Err(error) => {
            let in_git = std::process::Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .current_dir(root)
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).trim() == "true"
                });
            if in_git {
                return Err(error);
            }
            // Standalone fixture flakes have no Git manifest. Freeze their
            // complete bounded tree instead, including symlink identities.
            let mut queue = vec![root.to_owned()];
            let mut entries = BTreeMap::new();
            let mut total_bytes = 0_u64;
            while let Some(directory) = queue.pop() {
                for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let path = entry.path();
                    let relative = path
                        .strip_prefix(root)
                        .map_err(|_| "invalid source path")?
                        .to_string_lossy()
                        .into_owned();
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
                    if metadata.file_type().is_symlink() {
                        entries.insert(
                            relative,
                            format!(
                                "symlink:{}",
                                fs::read_link(&path)
                                    .map_err(|error| error.to_string())?
                                    .display()
                            ),
                        );
                    } else if metadata.is_dir() {
                        queue.push(path);
                        entries.insert(relative, "directory".to_owned());
                    } else if metadata.is_file() {
                        total_bytes = total_bytes
                            .checked_add(metadata.len())
                            .ok_or("source identity size overflow")?;
                        if total_bytes > 64 * 1024 * 1024 {
                            return Err("standalone source tree exceeds the 64 MiB interaction snapshot limit".to_owned());
                        }
                        let digest =
                            nxr_core::digest_file(&path).map_err(|error| error.to_string())?;
                        #[cfg(unix)]
                        let digest = {
                            use std::os::unix::fs::PermissionsExt;
                            format!("{}:{digest}", metadata.permissions().mode() & 0o777)
                        };
                        entries.insert(relative, digest);
                    } else {
                        return Err(
                            "standalone interaction source contains an unsupported file kind"
                                .to_owned(),
                        );
                    }
                    if entries.len() > 4096 {
                        return Err(
                            "standalone source tree exceeds the interaction entry limit".to_owned()
                        );
                    }
                }
            }
            Ok(format!("tree_{}", json_digest(&entries)?))
        }
    }
}
fn roots(plan: &ExecutionPlan) -> Vec<String> {
    if plan.roots.is_empty() {
        vec![plan.root.clone()]
    } else {
        plan.roots.clone()
    }
}
fn json_digest(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| "interaction identity cannot be serialized".to_owned())?;
    Ok(nxr_task_digest(&bytes))
}
fn nxr_task_digest(bytes: &[u8]) -> String {
    nxr_task::workflow_definition_digest(bytes)
        .trim_start_matches("wf_")
        .to_owned()
}

fn read_descriptor(directory: &Path, session_id: &str) -> Result<FrozenInvocation, String> {
    if !session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid interaction session id".to_owned());
    }
    serde_json::from_slice(&read_bounded(
        &directory.join(format!("{session_id}.invocation.json")),
    )?)
    .map_err(|_| "interaction invocation is corrupt".to_owned())
}
fn ensure_regular_or_absent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            Err("interaction sidecar must be a regular file".to_owned())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    ensure_regular_or_absent(path)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err("interaction sidecar exceeds size limit".to_owned());
    }
    Ok(bytes)
}
fn write_private(path: &Path, value: &impl Serialize) -> Result<(), String> {
    ensure_regular_or_absent(path)?;
    let bytes = serde_json::to_vec(value)
        .map_err(|_| "interaction sidecar serialization failed".to_owned())?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err("interaction sidecar exceeds size limit".to_owned());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("interaction directory missing")?)
            .map_err(|error| error.to_string())?;
    temporary
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error.to_string())?;
    #[cfg(unix)]
    File::open(path.parent().unwrap())
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}
fn write_or_verify_private(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if path.exists() {
        let expected = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if read_bounded(path)? != expected {
            return Err("frozen interaction inputs changed".to_owned());
        }
        Ok(())
    } else {
        write_private(path, value)
    }
}
fn quiet_runner() -> RunnerOutput {
    RunnerOutput::new(crate::output_options::OutputOptions::new(
        1,
        0,
        true,
        true,
        crate::output_options::ColorWhen::Never,
        crate::output_options::LogFormat::Plain,
    ))
}

pub fn run_cli(
    flake: Option<&str>,
    action: &InteractionCommand,
    runner: RunnerOutput,
) -> Result<i32, String> {
    let invocation = crate::commands::common::current_invocation_directory()
        .map_err(|error| error.to_string())?;
    let selection =
        crate::flake::resolve_flake(flake, &invocation).map_err(|error| error.to_string())?;
    let root = selection
        .local_root
        .ok_or_else(|| "interaction requires a local workspace".to_owned())?;
    let socket = nxr_core::daemon_socket_path();
    let value = match action {
        InteractionCommand::Inspect { session } => inspect_session(root.as_std_path(), session)?,
        InteractionCommand::Answer {
            session,
            request,
            revision,
            value,
            value_file,
            observation_digest,
        } => {
            let value = match (value, value_file) {
                (Some(value), None) => serde_json::from_str(value)
                    .map_err(|_| "answer must be valid JSON".to_owned())?,
                (None, Some(path)) => serde_json::from_slice(&read_bounded(path)?)
                    .map_err(|_| "answer file must contain JSON".to_owned())?,
                _ => return Err("supply exactly one of --value or --value-file".to_owned()),
            };
            answer_session(
                root.as_std_path(),
                session,
                request,
                *revision,
                value,
                observation_digest.clone(),
            )?
        }
        InteractionCommand::Prompt { session, revision } => {
            prompt_session(root.as_std_path(), session, *revision)?
        }
        InteractionCommand::Inbox { resume, revision } => {
            if let Some(session) = resume {
                let (store, _, authority) = store_for(root.as_std_path())?;
                let current = store
                    .get(session, &authority, now_secs())
                    .map_err(|error| error.to_string())?;
                let revision = revision.unwrap_or(current.revision);
                // Reuses the durable lease path: an already-acknowledged launch
                // returns Existing and never starts a second supervisor run.
                resume_session(root.as_std_path(), session, revision, &socket, runner)?
            } else {
                inbox_sessions(root.as_std_path())?
            }
        }
        InteractionCommand::Resume { session, revision } => {
            resume_session(root.as_std_path(), session, *revision, &socket, runner)?
        }
        InteractionCommand::Cancel { session, revision } => {
            cancel_session(root.as_std_path(), session, *revision, &socket)?
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_task::interaction::{CreateInteraction, InteractionCompletion};

    fn descriptor() -> FrozenInvocation {
        let tasks = BTreeMap::from([("test".to_owned(), nxr_task::TaskDefinition::new("test"))]);
        let plan =
            nxr_task::build_execution_plan(&tasks, "test", FailurePolicy::FailFast, None).unwrap();
        FrozenInvocation {
            schema_version: DESCRIPTOR_SCHEMA.to_owned(),
            workspace_root: PathBuf::from("/workspace"),
            invocation_directory: PathBuf::from("/workspace/subdir"),
            source_identity: "src_fixture".to_owned(),
            document_identity: "doc_fixture".to_owned(),
            system: "x86_64-linux".to_owned(),
            plan,
            operation_ids: vec!["workspace:task:test".to_owned()],
            supplied: BTreeMap::new(),
            caller_parameters: BTreeSet::new(),
            executable: PathBuf::from("/nix/store/runner/bin/nxr"),
            executable_digest: "exe_fixture".to_owned(),
            flake: "path:/workspace".to_owned(),
            nix_override: None,
            root: false,
            cwd: None,
            shell: None,
            shell_mode: "smart".to_owned(),
            clean_env: false,
            keep_env: Vec::new(),
            unset_env: Vec::new(),
            jobs: 2,
            keep_going: false,
            output_mode: Some("compact".to_owned()),
            events_jsonl: false,
            reports: Vec::new(),
            log_dir: None,
            context_override: Some("operator".to_owned()),
            offline: false,
            accept_flake_config: false,
            no_write_lock_file: true,
            effect_class: None,
        }
    }
    fn ready(store: &InteractionStore) -> InteractionSession {
        let session = store
            .create(CreateInteraction {
                operation_id: "workspace:task:test".to_owned(),
                caller_authority: "test-user".to_owned(),
                requests: Vec::new(),
                now: now_secs(),
                ttl_secs: 1000,
            })
            .unwrap();
        store
            .ready(
                &session.session_id,
                session.revision,
                "test-user",
                &"a".repeat(64),
                now_secs(),
            )
            .unwrap()
    }

    #[test]
    fn resumed_command_keeps_input_flags_before_roots_and_values_out_of_argv() {
        let command = command_for_inputs(&descriptor(), Path::new("/state/values.json")).unwrap();
        let task = command.iter().position(|arg| arg == "task").unwrap();
        let inputs = command.iter().position(|arg| arg == "--inputs").unwrap();
        let root = command.iter().position(|arg| arg == "test").unwrap();
        assert!(task < inputs && inputs < root);
        assert!(command.iter().any(|arg| arg == "--non-interactive"));
        assert_eq!(command[inputs + 1], "/state/values.json");
        assert!(!command.iter().any(|arg| arg.starts_with("NXR_PARAM_")));
        assert_eq!(
            descriptor().invocation_directory,
            PathBuf::from("/workspace/subdir")
        );
    }

    #[test]
    fn fake_launcher_runs_once_and_repeated_resume_returns_existing_identity() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = ready(&store);
        let launches = std::cell::Cell::new(0);
        let first = dispatch_reserved(
            &store,
            &session,
            "test-user",
            &"a".repeat(64),
            "run-fixed",
            || {
                launches.set(launches.get() + 1);
                Ok("run-fixed".to_owned())
            },
        )
        .unwrap();
        assert_eq!(first["started"], true);
        let second = dispatch_reserved(
            &store,
            &session,
            "test-user",
            &"a".repeat(64),
            "run-fixed",
            || {
                launches.set(launches.get() + 1);
                Ok("run-fixed".to_owned())
            },
        )
        .unwrap();
        assert_eq!(second["started"], false);
        assert_eq!(launches.get(), 1);
    }

    #[test]
    fn launch_failure_stays_uncertain_and_never_retries_a_process() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = ready(&store);
        let first = dispatch_reserved(
            &store,
            &session,
            "test-user",
            &"a".repeat(64),
            "run-fixed",
            || Err("lost acknowledgement".to_owned()),
        )
        .unwrap();
        assert_eq!(first["status"], "uncertain");
        let second = dispatch_reserved(
            &store,
            &session,
            "test-user",
            &"a".repeat(64),
            "run-fixed",
            || panic!("uncertain dispatch must not launch"),
        )
        .unwrap();
        assert_eq!(second["started"], false);
        assert_eq!(
            second["session"]["execution_lease"]["supervisor_run_id"],
            "run-fixed"
        );
    }

    #[test]
    fn cancelled_input_collection_never_calls_launcher() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = ready(&store);
        let cancelled = store
            .cancel(
                &session.session_id,
                session.revision,
                "test-user",
                now_secs(),
            )
            .unwrap();
        assert!(
            dispatch_reserved(
                &store,
                &cancelled,
                "test-user",
                &"a".repeat(64),
                "run-fixed",
                || panic!("cancelled input must not launch")
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_secret_values_are_rejected_before_creating_input_files() {
        let document=nxr_task::parse_task_document(&json!({"schema_version":3,"tasks":{"test":{"app":"test","parameters":{"credential":{"type":"secret_ref"}}}}})).unwrap();
        let plan =
            nxr_task::build_execution_plan(&document.tasks, "test", FailurePolicy::FailFast, None)
                .unwrap();
        let sentinel = "RAW_SECRET_SENTINEL";
        let result = input_argument_file(
            Some(&json!({"test":{"credential":sentinel}})),
            &document,
            &plan,
            &BTreeMap::new(),
        );
        assert!(result.is_err());
        assert!(!result.err().unwrap().contains(sentinel));
    }

    #[test]
    fn frozen_input_sidecars_are_immutable_and_private() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("values.json");
        write_or_verify_private(&path, &json!({"public":"first"})).unwrap();
        write_or_verify_private(&path, &json!({"public":"first"})).unwrap();
        assert!(write_or_verify_private(&path, &json!({"public":"second"})).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn parameter_prompt_hides_secret_defaults() {
        use nxr_task::interaction::InputRequest;
        use nxr_task::{TaskParameter, TaskParameterType};
        let request = InputRequest {
            request_id: "r".into(),
            node_id: "t".into(),
            parameter_name: "token".into(),
            declaration: TaskParameter {
                param_type: TaskParameterType::SecretRef,
                default: Some(json!("nxr-secret-value")),
                values: None,
                constraints: None,
            },
            observation_digest: None,
            revision: 0,
        };
        let prompt = present::format_parameter_prompt(&request);
        assert!(!prompt.contains("nxr-secret"));
    }

    #[test]
    fn terminal_states_do_not_get_reported_as_ready_to_execute() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = ready(&store);
        let result = dispatch_reserved(
            &store,
            &session,
            "test-user",
            &"a".repeat(64),
            "run-fixed",
            || Ok("run-fixed".to_owned()),
        )
        .unwrap();
        let lease = result["session"]["execution_lease"]["lease_id"]
            .as_str()
            .unwrap();
        let completed = store
            .finish(
                &session.session_id,
                lease,
                "test-user",
                InteractionCompletion::Succeeded,
            )
            .unwrap();
        assert_eq!(input_required(&completed)["status"], "completed");
    }
}
