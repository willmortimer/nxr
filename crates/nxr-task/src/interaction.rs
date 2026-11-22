//! Durable typed input sessions. The supervisor remains the process owner.
//!
//! An execution reservation is persisted before the caller receives launch
//! permission. A repeated reservation never grants launch permission again.
//! If launch acknowledgement is lost, reconciliation must inspect the existing
//! supervisor attempt; no lease timeout implicitly authorizes another process.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::parameters::{normalize_parameter_json, validate_task_parameters};
use crate::schema::TaskParameter;

pub const INTERACTION_SCHEMA: &str = "nxr-interaction-v1";
pub const DEFAULT_INTERACTION_TTL_SECS: u64 = 24 * 60 * 60;
const MAX_REQUESTS: usize = 128;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_ANSWER_BYTES: usize = 64 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionState {
    CollectingInputs,
    WaitingForInput,
    Resolving,
    PlanReady,
    WaitingForAuthorization,
    ReadyToExecute,
    Executing,
    Completed,
    Cancelled,
    Expired,
    Uncertain,
}

impl InteractionState {
    fn expires_before_execution(self) -> bool {
        matches!(
            self,
            Self::CollectingInputs
                | Self::WaitingForInput
                | Self::Resolving
                | Self::PlanReady
                | Self::WaitingForAuthorization
                | Self::ReadyToExecute
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewInputRequest {
    pub request_id: String,
    pub node_id: String,
    pub parameter_name: String,
    pub declaration: TaskParameter,
    pub observation_digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRequest {
    pub request_id: String,
    pub node_id: String,
    pub parameter_name: String,
    pub declaration: TaskParameter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_digest: Option<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAnswer {
    pub session_id: String,
    pub request_id: String,
    pub revision: u64,
    /// The adapter binds this to its authenticated caller; this field grants nothing.
    pub caller_authority: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedAnswer {
    pub request: InputRequest,
    pub caller_authority: String,
    pub value: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLease {
    pub lease_id: String,
    pub reserved_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_run_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionCompletion {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionSession {
    pub schema_version: String,
    pub session_id: String,
    /// Requested catalog operation; not a fabricated pre-resolution execution ID.
    pub operation_id: String,
    pub caller_authority: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<String>,
    pub state: InteractionState,
    pub pending_requests: Vec<InputRequest>,
    pub accepted_answers: BTreeMap<String, AcceptedAnswer>,
    pub revision: u64,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_lease: Option<ExecutionLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_outcome: Option<InteractionCompletion>,
}

impl InteractionSession {
    #[must_use]
    pub fn answer_values(&self) -> BTreeMap<String, BTreeMap<String, Value>> {
        let mut values: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        for accepted in self.accepted_answers.values() {
            values
                .entry(accepted.request.node_id.clone())
                .or_default()
                .insert(
                    accepted.request.parameter_name.clone(),
                    accepted.value.clone(),
                );
        }
        values
    }
}

#[derive(Clone, Debug)]
pub struct CreateInteraction {
    pub operation_id: String,
    pub caller_authority: String,
    pub requests: Vec<NewInputRequest>,
    pub now: u64,
    pub ttl_secs: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerOutcome {
    pub session: InteractionSession,
    pub repeated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResumeOutcome {
    /// Only this result authorizes the adapter to ask the supervisor to launch.
    Reserved {
        session: InteractionSession,
        lease_id: String,
    },
    /// Inspect/reconcile the existing reservation; never launch another process.
    Existing { session: InteractionSession },
}

/// Frontends gather ordinary values or security-reference envelopes. A frontend
/// cannot authorize a domain operation or launch a process through this trait.
pub trait InteractionFrontend {
    fn answer(&mut self, request: &InputRequest) -> Result<Option<Value>, InteractionError>;
}

#[derive(Debug, Error)]
pub enum InteractionError {
    #[error("interaction storage: {0}")]
    Storage(#[from] std::io::Error),
    #[error("invalid interaction: {0}")]
    Invalid(String),
    #[error("interaction caller is not authorized for this session")]
    Unauthorized,
    #[error("interaction revision is stale (expected {expected}, got {provided})")]
    StaleRevision { expected: u64, provided: u64 },
    #[error("interaction response conflicts with the accepted answer")]
    ConflictingAnswer,
    #[error("interaction observation changed; refresh the request")]
    StaleObservation,
    #[error("interaction state does not permit this operation")]
    InvalidState,
    #[error("interaction session has expired")]
    Expired,
    #[error("interaction plan digest does not match the frozen plan")]
    PlanMismatch,
}

#[derive(Clone, Debug)]
pub struct InteractionStore {
    directory: PathBuf,
}

impl InteractionStore {
    /// The caller supplies a private per-workspace state directory, not an input-file path.
    pub fn new(directory: &Path) -> Result<Self, InteractionError> {
        if !directory.is_absolute()
            || directory
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(invalid(
                "session store requires an absolute path without parent traversal",
            ));
        }
        reject_existing_symlink(directory)?;
        fs::create_dir_all(directory)?;
        // Canonical ancestors permit platform aliases such as macOS /var,
        // while the supplied state directory itself may not be a symlink.
        let directory = fs::canonicalize(directory)?;
        reject_symlink_components(&directory)?;
        private_directory(&directory)?;
        Ok(Self { directory })
    }

    pub fn create(
        &self,
        create: CreateInteraction,
    ) -> Result<InteractionSession, InteractionError> {
        validate_identifier(&create.operation_id)?;
        validate_identifier(&create.caller_authority)?;
        if create.ttl_secs == 0 {
            return Err(invalid("session lifetime must be positive"));
        }
        let expires_at = create
            .now
            .checked_add(create.ttl_secs)
            .ok_or_else(|| invalid("session lifetime overflows"))?;
        if create.requests.len() > MAX_REQUESTS {
            return Err(invalid("too many interaction requests"));
        }
        let mut ids = BTreeSet::new();
        let mut parameters = BTreeSet::new();
        let mut requests = Vec::new();
        for request in create.requests {
            validate_identifier(&request.request_id)?;
            validate_identifier(&request.node_id)?;
            validate_identifier(&request.parameter_name)?;
            validate_observation(request.observation_digest.as_deref())?;
            if !ids.insert(request.request_id.clone())
                || !parameters.insert((request.node_id.clone(), request.parameter_name.clone()))
            {
                return Err(invalid("duplicate request or node parameter"));
            }
            validate_task_parameters(
                &request.node_id,
                &BTreeMap::from([(request.parameter_name.clone(), request.declaration.clone())]),
            )
            .map_err(|_| invalid("invalid interaction parameter declaration"))?;
            requests.push(InputRequest {
                request_id: request.request_id,
                node_id: request.node_id,
                parameter_name: request.parameter_name,
                declaration: public_declaration(request.declaration),
                observation_digest: request.observation_digest,
                revision: 1,
            });
        }
        let seed = tempfile::Builder::new()
            .prefix(".session-seed-")
            .tempfile_in(&self.directory)?;
        let session_id = format!(
            "session-{}",
            hex_digest(seed.path().as_os_str().as_encoded_bytes())
        );
        let session = InteractionSession {
            schema_version: INTERACTION_SCHEMA.to_owned(),
            session_id: session_id.clone(),
            operation_id: create.operation_id,
            caller_authority: create.caller_authority,
            plan_digest: None,
            state: if requests.is_empty() {
                InteractionState::Resolving
            } else {
                InteractionState::WaitingForInput
            },
            pending_requests: requests,
            accepted_answers: BTreeMap::new(),
            revision: 1,
            created_at: create.now,
            expires_at,
            execution_lease: None,
            completion_outcome: None,
        };
        self.with_lock(&session_id, || {
            if self.session_path(&session_id).exists() {
                return Err(invalid("interaction identity collision"));
            }
            self.persist(&session)?;
            Ok(session)
        })
    }

    pub fn get(
        &self,
        session_id: &str,
        authority: &str,
        now: u64,
    ) -> Result<InteractionSession, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            if expire(&mut session, now)? {
                self.persist(&session)?;
            }
            Ok(session)
        })
    }

    pub fn answer(
        &self,
        answer: InputAnswer,
        authority: &str,
        now: u64,
    ) -> Result<AnswerOutcome, InteractionError> {
        self.with_lock(&answer.session_id, || {
            let mut session = self.load(&answer.session_id)?;
            authorize(&session, authority)?;
            if answer.caller_authority != authority {
                return Err(InteractionError::Unauthorized);
            }
            if serde_json::to_vec(&answer.value)
                .map_err(|_| invalid("invalid response value"))?
                .len()
                > MAX_ANSWER_BYTES
            {
                return Err(invalid("interaction answer is too large"));
            }
            if let Some(accepted) = session.accepted_answers.get(&answer.request_id) {
                let normalized = normalize_parameter_json(
                    &accepted.request.node_id,
                    &accepted.request.parameter_name,
                    &accepted.request.declaration,
                    &answer.value,
                )
                .map_err(|_| invalid("answer does not match its declared type"))?;
                if accepted.request.revision == answer.revision
                    && accepted.caller_authority == authority
                    && accepted.request.observation_digest == answer.observation_digest
                    && accepted.value == normalized
                {
                    if expire(&mut session, now)? {
                        self.persist(&session)?;
                    }
                    return Ok(AnswerOutcome {
                        session,
                        repeated: true,
                    });
                }
                return Err(InteractionError::ConflictingAnswer);
            }
            self.require_unexpired(&mut session, now)?;
            if session.state != InteractionState::WaitingForInput {
                return Err(InteractionError::InvalidState);
            }
            check_revision(&session, answer.revision)?;
            let position = session
                .pending_requests
                .iter()
                .position(|request| request.request_id == answer.request_id)
                .ok_or_else(|| invalid("unknown input request"))?;
            let request = &session.pending_requests[position];
            if request.observation_digest != answer.observation_digest {
                return Err(InteractionError::StaleObservation);
            }
            let value = normalize_parameter_json(
                &request.node_id,
                &request.parameter_name,
                &request.declaration,
                &answer.value,
            )
            .map_err(|_| invalid("answer does not match its declared type"))?;
            let request = session.pending_requests.remove(position);
            session.accepted_answers.insert(
                answer.request_id,
                AcceptedAnswer {
                    request,
                    caller_authority: authority.to_owned(),
                    value,
                },
            );
            session.state = if session.pending_requests.is_empty() {
                InteractionState::Resolving
            } else {
                InteractionState::WaitingForInput
            };
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(AnswerOutcome {
                session,
                repeated: false,
            })
        })
    }

    /// Replaces choices after real discovery changes. Previously accepted values
    /// for that parameter are invalidated, and every outstanding revision changes.
    pub fn refresh_request(
        &self,
        session_id: &str,
        expected_revision: u64,
        authority: &str,
        new_request: NewInputRequest,
        now: u64,
    ) -> Result<InteractionSession, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            self.require_unexpired(&mut session, now)?;
            check_revision(&session, expected_revision)?;
            if session.execution_lease.is_some() || !session.state.expires_before_execution() {
                return Err(InteractionError::InvalidState);
            }
            validate_identifier(&new_request.request_id)?;
            validate_identifier(&new_request.node_id)?;
            validate_observation(new_request.observation_digest.as_deref())?;
            validate_task_parameters(
                &new_request.node_id,
                &BTreeMap::from([(
                    new_request.parameter_name.clone(),
                    new_request.declaration.clone(),
                )]),
            )
            .map_err(|_| invalid("invalid interaction parameter declaration"))?;
            let old_pending = session
                .pending_requests
                .iter()
                .find(|request| request.request_id == new_request.request_id);
            let old_accepted = session
                .accepted_answers
                .get(&new_request.request_id)
                .map(|answer| &answer.request);
            let previous = old_pending
                .or(old_accepted)
                .ok_or_else(|| invalid("unknown input request"))?;
            if previous.node_id != new_request.node_id
                || previous.parameter_name != new_request.parameter_name
            {
                return Err(invalid("request identity cannot be rebound"));
            }
            session
                .pending_requests
                .retain(|request| request.request_id != new_request.request_id);
            session.accepted_answers.remove(&new_request.request_id);
            session.pending_requests.push(InputRequest {
                request_id: new_request.request_id,
                node_id: new_request.node_id,
                parameter_name: new_request.parameter_name,
                declaration: public_declaration(new_request.declaration),
                observation_digest: new_request.observation_digest,
                revision: session.revision,
            });
            session.plan_digest = None;
            session.state = InteractionState::WaitingForInput;
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(session)
        })
    }

    /// Freeze a resolved plan after external project and domain checks. This
    /// method does not verify or manufacture an NXI/NXD authorization grant.
    pub fn ready(
        &self,
        session_id: &str,
        expected_revision: u64,
        authority: &str,
        plan_digest: &str,
        now: u64,
    ) -> Result<InteractionSession, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            self.require_unexpired(&mut session, now)?;
            check_revision(&session, expected_revision)?;
            validate_plan_digest(plan_digest)?;
            if !session.pending_requests.is_empty()
                || !matches!(
                    session.state,
                    InteractionState::Resolving
                        | InteractionState::PlanReady
                        | InteractionState::WaitingForAuthorization
                        | InteractionState::ReadyToExecute
                )
            {
                return Err(InteractionError::InvalidState);
            }
            if session
                .plan_digest
                .as_deref()
                .is_some_and(|digest| digest != plan_digest)
            {
                return Err(InteractionError::PlanMismatch);
            }
            session.plan_digest = Some(plan_digest.to_owned());
            session.state = InteractionState::ReadyToExecute;
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(session)
        })
    }

    pub fn cancel(
        &self,
        session_id: &str,
        expected_revision: u64,
        authority: &str,
        now: u64,
    ) -> Result<InteractionSession, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            if session.state == InteractionState::Cancelled {
                return Ok(session);
            }
            self.require_unexpired(&mut session, now)?;
            check_revision(&session, expected_revision)?;
            if session.execution_lease.is_some() || !session.state.expires_before_execution() {
                return Err(InteractionError::InvalidState);
            }
            session.state = InteractionState::Cancelled;
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(session)
        })
    }

    pub fn resume(
        &self,
        session_id: &str,
        expected_revision: u64,
        authority: &str,
        plan_digest: &str,
        now: u64,
    ) -> Result<ResumeOutcome, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            validate_plan_digest(plan_digest)?;
            if session.plan_digest.as_deref() != Some(plan_digest) {
                return Err(InteractionError::PlanMismatch);
            }
            if session.execution_lease.is_some() {
                if session.state == InteractionState::Executing
                    && session
                        .execution_lease
                        .as_ref()
                        .is_some_and(|lease| lease.supervisor_run_id.is_none())
                {
                    session.state = InteractionState::Uncertain;
                    bump_revision(&mut session)?;
                    self.persist(&session)?;
                }
                return Ok(ResumeOutcome::Existing { session });
            }
            self.require_unexpired(&mut session, now)?;
            check_revision(&session, expected_revision)?;
            if session.state != InteractionState::ReadyToExecute {
                return Err(InteractionError::InvalidState);
            }
            let lease_id = format!(
                "lease-{}",
                hex_digest(format!("{}:{}:{now}", session.session_id, session.revision).as_bytes())
            );
            session.execution_lease = Some(ExecutionLease {
                lease_id: lease_id.clone(),
                reserved_at: now,
                supervisor_run_id: None,
            });
            session.state = InteractionState::Executing;
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(ResumeOutcome::Reserved { session, lease_id })
        })
    }

    /// Acknowledge the supervisor identity for the already reserved launch.
    /// Binding the same acknowledgement again is idempotent.
    pub fn bind_execution(
        &self,
        session_id: &str,
        lease_id: &str,
        authority: &str,
        supervisor_run_id: &str,
    ) -> Result<InteractionSession, InteractionError> {
        validate_segment(supervisor_run_id)?;
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            if !matches!(
                session.state,
                InteractionState::Executing | InteractionState::Uncertain
            ) {
                return Err(InteractionError::InvalidState);
            }
            let lease = session
                .execution_lease
                .as_mut()
                .ok_or(InteractionError::InvalidState)?;
            if lease.lease_id != lease_id {
                return Err(InteractionError::Unauthorized);
            }
            if let Some(existing) = &lease.supervisor_run_id {
                if existing != supervisor_run_id {
                    return Err(invalid(
                        "execution lease is already bound to another supervisor run",
                    ));
                }
                return Ok(session);
            }
            lease.supervisor_run_id = Some(supervisor_run_id.to_owned());
            session.state = InteractionState::Executing;
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(session)
        })
    }

    pub fn mark_uncertain(
        &self,
        session_id: &str,
        lease_id: &str,
        authority: &str,
    ) -> Result<InteractionSession, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            if !matches!(
                session.state,
                InteractionState::Executing | InteractionState::Uncertain
            ) || session
                .execution_lease
                .as_ref()
                .is_none_or(|lease| lease.lease_id != lease_id)
            {
                return Err(InteractionError::InvalidState);
            }
            if session.state != InteractionState::Uncertain {
                session.state = InteractionState::Uncertain;
                bump_revision(&mut session)?;
                self.persist(&session)?;
            }
            Ok(session)
        })
    }

    /// The adapter calls this only after obtaining an authoritative supervisor result.
    pub fn finish(
        &self,
        session_id: &str,
        lease_id: &str,
        authority: &str,
        outcome: InteractionCompletion,
    ) -> Result<InteractionSession, InteractionError> {
        self.with_lock(session_id, || {
            let mut session = self.load(session_id)?;
            authorize(&session, authority)?;
            if !session.execution_lease.as_ref().is_some_and(|lease| {
                lease.lease_id == lease_id && lease.supervisor_run_id.is_some()
            }) {
                return Err(InteractionError::InvalidState);
            }
            if let Some(existing) = session.completion_outcome {
                if existing != outcome {
                    return Err(invalid(
                        "execution completion conflicts with recorded outcome",
                    ));
                }
                return Ok(session);
            }
            if !matches!(
                session.state,
                InteractionState::Executing | InteractionState::Uncertain
            ) {
                return Err(InteractionError::InvalidState);
            }
            session.state = if outcome == InteractionCompletion::Cancelled {
                InteractionState::Cancelled
            } else {
                InteractionState::Completed
            };
            session.completion_outcome = Some(outcome);
            bump_revision(&mut session)?;
            self.persist(&session)?;
            Ok(session)
        })
    }

    fn require_unexpired(
        &self,
        session: &mut InteractionSession,
        now: u64,
    ) -> Result<(), InteractionError> {
        if expire(session, now)? {
            self.persist(session)?;
        }
        if session.state == InteractionState::Expired {
            return Err(InteractionError::Expired);
        }
        Ok(())
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.directory.join(format!("{session_id}.json"))
    }

    fn with_lock<T>(
        &self,
        session_id: &str,
        operation: impl FnOnce() -> Result<T, InteractionError>,
    ) -> Result<T, InteractionError> {
        validate_segment(session_id)?;
        reject_symlink_components(&self.directory)?;
        let path = self.directory.join(format!("{session_id}.lock"));
        reject_nonregular_record(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(&path)?;
        private_file(&lock)?;
        FileExt::lock_exclusive(&lock)?;
        let result = operation();
        let unlocked = FileExt::unlock(&lock);
        match result {
            Ok(value) => {
                unlocked?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }

    fn load(&self, session_id: &str) -> Result<InteractionSession, InteractionError> {
        let path = self.session_path(session_id);
        reject_nonregular_record(&path)?;
        let file = File::open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("interaction record is not a file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("interaction record is too large"));
        }
        let session: InteractionSession =
            serde_json::from_slice(&bytes).map_err(|_| invalid("interaction record is corrupt"))?;
        validate_stored_session(&session, session_id)?;
        Ok(session)
    }

    fn persist(&self, session: &InteractionSession) -> Result<(), InteractionError> {
        validate_stored_session(session, &session.session_id)?;
        let path = self.session_path(&session.session_id);
        reject_existing_symlink(&path)?;
        let bytes = serde_json::to_vec(session)
            .map_err(|_| invalid("interaction record cannot be serialized"))?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("interaction record is too large"));
        }
        let mut temporary = tempfile::Builder::new()
            .prefix(".interaction-")
            .tempfile_in(&self.directory)?;
        private_file(temporary.as_file())?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .map_err(|error| InteractionError::Storage(error.error))?;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

fn validate_stored_session(
    session: &InteractionSession,
    session_id: &str,
) -> Result<(), InteractionError> {
    if session.schema_version != INTERACTION_SCHEMA
        || session.session_id != session_id
        || session.revision == 0
        || session.expires_at <= session.created_at
        || session.pending_requests.len() + session.accepted_answers.len() > MAX_REQUESTS
    {
        return Err(invalid("interaction record invariants do not hold"));
    }
    validate_identifier(&session.operation_id)?;
    validate_identifier(&session.caller_authority)?;
    if let Some(digest) = &session.plan_digest {
        validate_plan_digest(digest)?;
    }
    let mut ids = BTreeSet::new();
    let mut parameters = BTreeSet::new();
    for request in session.pending_requests.iter().chain(
        session
            .accepted_answers
            .values()
            .map(|answer| &answer.request),
    ) {
        validate_identifier(&request.request_id)?;
        validate_identifier(&request.node_id)?;
        validate_observation(request.observation_digest.as_deref())?;
        if request.revision == 0
            || request.revision > session.revision
            || !ids.insert(request.request_id.clone())
            || !parameters.insert((request.node_id.clone(), request.parameter_name.clone()))
            || declaration_has_sensitive_default(&request.declaration)
        {
            return Err(invalid("interaction request invariants do not hold"));
        }
        validate_task_parameters(
            &request.node_id,
            &BTreeMap::from([(request.parameter_name.clone(), request.declaration.clone())]),
        )
        .map_err(|_| invalid("invalid persisted input declaration"))?;
    }
    for (id, accepted) in &session.accepted_answers {
        if id != &accepted.request.request_id
            || accepted.caller_authority != session.caller_authority
        {
            return Err(invalid("interaction answer identity mismatch"));
        }
        let normalized = normalize_parameter_json(
            &accepted.request.node_id,
            &accepted.request.parameter_name,
            &accepted.request.declaration,
            &accepted.value,
        )
        .map_err(|_| invalid("persisted answer violates declared type"))?;
        if normalized != accepted.value {
            return Err(invalid("persisted answer is not canonical"));
        }
    }
    if matches!(
        session.state,
        InteractionState::ReadyToExecute
            | InteractionState::Executing
            | InteractionState::Completed
            | InteractionState::Uncertain
    ) && (!session.pending_requests.is_empty() || session.plan_digest.is_none())
    {
        return Err(invalid("execution state has unresolved inputs or no plan"));
    }
    if matches!(
        session.state,
        InteractionState::Executing | InteractionState::Completed | InteractionState::Uncertain
    ) && session.execution_lease.is_none()
    {
        return Err(invalid("execution state has no durable lease"));
    }
    Ok(())
}

fn public_declaration(mut declaration: TaskParameter) -> TaskParameter {
    if crate::parameters::parameter_contains_secret_reference(&declaration) {
        declaration.default = None;
    }
    if let Some(constraints) = declaration.constraints.as_mut() {
        for nested in constraints.properties.values_mut() {
            *nested = public_declaration(nested.clone());
        }
    }
    declaration
}

fn declaration_has_sensitive_default(declaration: &TaskParameter) -> bool {
    (crate::parameters::parameter_contains_secret_reference(declaration)
        && declaration.default.is_some())
        || declaration.constraints.as_ref().is_some_and(|constraints| {
            constraints
                .properties
                .values()
                .any(declaration_has_sensitive_default)
        })
}

fn authorize(session: &InteractionSession, authority: &str) -> Result<(), InteractionError> {
    if session.caller_authority == authority {
        Ok(())
    } else {
        Err(InteractionError::Unauthorized)
    }
}

fn check_revision(session: &InteractionSession, provided: u64) -> Result<(), InteractionError> {
    if session.revision == provided {
        Ok(())
    } else {
        Err(InteractionError::StaleRevision {
            expected: session.revision,
            provided,
        })
    }
}

fn bump_revision(session: &mut InteractionSession) -> Result<(), InteractionError> {
    session.revision = session
        .revision
        .checked_add(1)
        .ok_or_else(|| invalid("interaction revision overflow"))?;
    for request in &mut session.pending_requests {
        request.revision = session.revision;
    }
    Ok(())
}

fn expire(session: &mut InteractionSession, now: u64) -> Result<bool, InteractionError> {
    if now >= session.expires_at && session.state.expires_before_execution() {
        session.state = InteractionState::Expired;
        bump_revision(session)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn validate_identifier(value: &str) -> Result<(), InteractionError> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES || value.chars().any(char::is_control)
    {
        Err(invalid("invalid interaction identifier"))
    } else {
        Ok(())
    }
}

fn validate_segment(value: &str) -> Result<(), InteractionError> {
    validate_identifier(value)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid("interaction identifier is not a safe path segment"));
    }
    Ok(())
}

fn validate_plan_digest(value: &str) -> Result<(), InteractionError> {
    if value.strip_prefix("sha256:").unwrap_or(value).len() != 64
        || !value
            .strip_prefix("sha256:")
            .unwrap_or(value)
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid("plan digest must be lowercase SHA-256"));
    }
    Ok(())
}

fn validate_observation(digest: Option<&str>) -> Result<(), InteractionError> {
    if let Some(digest) = digest {
        validate_plan_digest(digest)?;
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn invalid(message: &str) -> InteractionError {
    InteractionError::Invalid(message.to_owned())
}

fn reject_existing_symlink(path: &Path) -> Result<(), InteractionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(invalid("symlink interaction paths are not allowed"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn reject_nonregular_record(path: &Path) -> Result<(), InteractionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => Err(invalid(
            "interaction record and lock paths must be regular files",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn reject_symlink_components(path: &Path) -> Result<(), InteractionError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        reject_existing_symlink(&current)?;
    }
    Ok(())
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), InteractionError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), InteractionError> {
    Ok(())
}
#[cfg(unix)]
fn private_file(file: &File) -> Result<(), InteractionError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
fn private_file(_file: &File) -> Result<(), InteractionError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AUTHORITY: &str = "local-user:test";
    fn request(id: &str, kind: &str) -> NewInputRequest {
        NewInputRequest {
            request_id: id.to_owned(),
            node_id: "prepare".to_owned(),
            parameter_name: id.to_owned(),
            declaration: serde_json::from_value(json!({"type":kind})).unwrap(),
            observation_digest: None,
        }
    }
    fn create(store: &InteractionStore, requests: Vec<NewInputRequest>) -> InteractionSession {
        store
            .create(CreateInteraction {
                operation_id: "operation:test".to_owned(),
                caller_authority: AUTHORITY.to_owned(),
                requests,
                now: 100,
                ttl_secs: 1000,
            })
            .unwrap()
    }
    fn answer(session: &InteractionSession, id: &str, value: Value) -> InputAnswer {
        InputAnswer {
            session_id: session.session_id.clone(),
            request_id: id.to_owned(),
            revision: session.revision,
            caller_authority: AUTHORITY.to_owned(),
            value,
            observation_digest: None,
        }
    }
    fn digest() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    #[test]
    fn restart_retains_pending_inputs_and_duplicate_answers_are_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(
            &store,
            vec![request("name", "string"), request("count", "integer")],
        );
        let original = answer(&session, "name", json!("host"));
        let result = store.answer(original.clone(), AUTHORITY, 101).unwrap();
        assert_eq!(result.session.pending_requests.len(), 1);
        let restarted = InteractionStore::new(directory.path()).unwrap();
        let repeated = restarted.answer(original.clone(), AUTHORITY, 102).unwrap();
        assert!(repeated.repeated);
        assert_eq!(repeated.session.revision, result.session.revision);
        let mut conflicting = original;
        conflicting.value = json!("other");
        assert!(matches!(
            restarted.answer(conflicting, AUTHORITY, 102),
            Err(InteractionError::ConflictingAnswer)
        ));
    }

    #[test]
    fn unauthorized_and_stale_responses_cannot_change_state() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(
            &store,
            vec![request("name", "string"), request("count", "integer")],
        );
        assert!(matches!(
            store.answer(answer(&session, "name", json!("host")), "other", 101),
            Err(InteractionError::Unauthorized)
        ));
        let result = store
            .answer(answer(&session, "name", json!("host")), AUTHORITY, 101)
            .unwrap();
        assert!(matches!(
            store.answer(answer(&session, "count", json!(2)), AUTHORITY, 102),
            Err(InteractionError::StaleRevision { .. })
        ));
        assert_eq!(
            store
                .get(&session.session_id, AUTHORITY, 102)
                .unwrap()
                .revision,
            result.session.revision
        );
    }

    #[test]
    fn discovery_refresh_invalidates_previously_accepted_answers() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let mut disk = request("disk", "string");
        disk.observation_digest = Some(digest());
        let session = create(&store, vec![disk.clone()]);
        let mut response = answer(&session, "disk", json!("disk-a"));
        response.observation_digest = Some(digest());
        let accepted = store.answer(response.clone(), AUTHORITY, 101).unwrap();
        disk.observation_digest = Some(format!("sha256:{}", "b".repeat(64)));
        let refreshed = store
            .refresh_request(
                &session.session_id,
                accepted.session.revision,
                AUTHORITY,
                disk,
                102,
            )
            .unwrap();
        assert!(refreshed.accepted_answers.is_empty());
        response.revision = refreshed.revision;
        assert!(matches!(
            store.answer(response, AUTHORITY, 103),
            Err(InteractionError::StaleObservation)
        ));
    }

    #[test]
    fn execution_lease_is_single_use_and_lost_acknowledgement_stays_uncertain() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(&store, Vec::new());
        let ready = store
            .ready(
                &session.session_id,
                session.revision,
                AUTHORITY,
                &digest(),
                101,
            )
            .unwrap();
        let reserved = store
            .resume(
                &session.session_id,
                ready.revision,
                AUTHORITY,
                &digest(),
                102,
            )
            .unwrap();
        let lease_id = match reserved {
            ResumeOutcome::Reserved { lease_id, .. } => lease_id,
            _ => panic!("first reservation"),
        };
        let retry = store
            .resume(
                &session.session_id,
                ready.revision,
                AUTHORITY,
                &digest(),
                103,
            )
            .unwrap();
        match retry {
            ResumeOutcome::Existing { session } => {
                assert_eq!(session.state, InteractionState::Uncertain)
            }
            _ => panic!("duplicate reservation"),
        }
        let bound = store
            .bind_execution(&session.session_id, &lease_id, AUTHORITY, "run-1")
            .unwrap();
        assert_eq!(bound.state, InteractionState::Executing);
        let terminal = store
            .finish(
                &session.session_id,
                &lease_id,
                AUTHORITY,
                InteractionCompletion::Succeeded,
            )
            .unwrap();
        assert_eq!(terminal.state, InteractionState::Completed);
        assert!(matches!(
            store
                .resume(
                    &session.session_id,
                    terminal.revision,
                    AUTHORITY,
                    &digest(),
                    104
                )
                .unwrap(),
            ResumeOutcome::Existing { .. }
        ));
    }

    #[test]
    fn concurrent_resume_returns_one_launch_permission() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(&store, Vec::new());
        let ready = store
            .ready(
                &session.session_id,
                session.revision,
                AUTHORITY,
                &digest(),
                101,
            )
            .unwrap();
        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let store = store.clone();
                    let ready = ready.clone();
                    scope.spawn(move || {
                        store
                            .resume(&ready.session_id, ready.revision, AUTHORITY, &digest(), 102)
                            .unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ResumeOutcome::Reserved { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn expiry_and_cancellation_never_create_an_execution_lease() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(&store, vec![request("name", "string")]);
        assert!(matches!(
            store.answer(answer(&session, "name", json!("host")), AUTHORITY, 1100),
            Err(InteractionError::Expired)
        ));
        let expired = store.get(&session.session_id, AUTHORITY, 1101).unwrap();
        assert_eq!(expired.state, InteractionState::Expired);
        assert!(expired.execution_lease.is_none());
        let session = create(&store, vec![request("name", "string")]);
        let cancelled = store
            .cancel(&session.session_id, session.revision, AUTHORITY, 101)
            .unwrap();
        assert_eq!(cancelled.state, InteractionState::Cancelled);
        assert!(cancelled.execution_lease.is_none());
    }

    #[test]
    fn secret_answers_accept_only_references_and_never_persist_refused_plaintext() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(&store, vec![request("credential", "secret_ref")]);
        let sentinel = "PRIVATE_SECRET_SENTINEL";
        let error = store
            .answer(
                answer(&session, "credential", json!(sentinel)),
                AUTHORITY,
                101,
            )
            .unwrap_err();
        assert!(!error.to_string().contains(sentinel));
        let accepted = store
            .answer(
                answer(
                    &session,
                    "credential",
                    json!({"ref":"ENV_CREDENTIAL","provider":"env"}),
                ),
                AUTHORITY,
                102,
            )
            .unwrap();
        let bytes = fs::read(store.session_path(&session.session_id)).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains(sentinel));
        assert_eq!(
            accepted.session.answer_values()["prepare"]["credential"]["ref"],
            json!("ENV_CREDENTIAL")
        );
    }

    #[test]
    fn traversal_and_symlink_records_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        assert!(store.get("../outside", AUTHORITY, 1).is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::os::unix::fs::symlink(outside.path(), store.session_path("session-symlink"))
                .unwrap();
            assert!(store.get("session-symlink", AUTHORITY, 1).is_err());
        }
    }
    #[test]
    fn sensitive_defaults_are_refused_and_storage_failure_never_returns_a_lease() {
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let mut secret = request("credential", "secret_ref");
        secret.declaration.default = Some(json!({"ref":"IMPLICIT_CREDENTIAL"}));
        assert!(
            store
                .create(CreateInteraction {
                    operation_id: "operation:test".to_owned(),
                    caller_authority: AUTHORITY.to_owned(),
                    requests: vec![secret],
                    now: 100,
                    ttl_secs: 1000,
                })
                .is_err()
        );
        let session = create(&store, Vec::new());
        let ready = store
            .ready(
                &session.session_id,
                session.revision,
                AUTHORITY,
                &digest(),
                101,
            )
            .unwrap();
        // Replace the private record with a directory to make the fallible storage
        // boundary reject a resume before granting any launch permission.
        fs::remove_file(store.session_path(&session.session_id)).unwrap();
        fs::create_dir(store.session_path(&session.session_id)).unwrap();
        assert!(
            store
                .resume(
                    &session.session_id,
                    ready.revision,
                    AUTHORITY,
                    &digest(),
                    102
                )
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn persisted_session_and_lock_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let store = InteractionStore::new(directory.path()).unwrap();
        let session = create(&store, vec![request("name", "string")]);
        assert_eq!(
            fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.session_path(&session.session_id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.directory.join(format!("{}.lock", session.session_id)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
