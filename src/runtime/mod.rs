// Runtime owns the public facade and shared internal state; extracted business logic lives in submodules.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

const CORE_EVENT_TABLE: &str = "CORE__events";
const FIXED_DB_RELATIVE_PATH: &str = "./.loopy/loopy.db";
const DEFAULT_INSTALLED_SKILL_RELATIVE_PATH: &str = ".loopy/installed-skills/loopy-submit-loop";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn setpgid(pid: i32, pgid: i32) -> i32;
}

mod api;

pub(crate) mod ops;
pub(crate) mod projection;
pub(crate) mod query;
pub(crate) mod roles;
pub(crate) mod system;

pub use self::api::*;

#[derive(Debug, Clone)]
pub struct Runtime {
    workspace_root: PathBuf,
    db_path_override: Option<PathBuf>,
    installed_skill_root_override: Option<PathBuf>,
}

impl Runtime {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_overrides(workspace_root, None, None)
    }

    pub fn with_installed_skill_root(
        workspace_root: impl Into<PathBuf>,
        installed_skill_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::with_overrides(workspace_root, None, Some(installed_skill_root.into()))
    }

    pub fn with_db_path_override(
        workspace_root: impl Into<PathBuf>,
        db_path_override: Option<PathBuf>,
    ) -> Result<Self> {
        Self::with_overrides(workspace_root, db_path_override, None)
    }

    fn with_overrides(
        workspace_root: impl Into<PathBuf>,
        db_path_override: Option<PathBuf>,
        installed_skill_root_override: Option<PathBuf>,
    ) -> Result<Self> {
        let runtime = Self {
            workspace_root: workspace_root.into(),
            db_path_override,
            installed_skill_root_override,
        };
        runtime.validate_db_path()?;
        Ok(runtime)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn db_path_override(&self) -> Option<&Path> {
        self.db_path_override.as_deref()
    }

    pub fn installed_skill_root(&self) -> PathBuf {
        self.installed_skill_root_override
            .clone()
            .unwrap_or_else(|| {
                self.workspace_root
                    .join(DEFAULT_INSTALLED_SKILL_RELATIVE_PATH)
            })
    }

    pub fn from_invocation_context_path(invocation_context_path: &Path) -> Result<Self> {
        let invocations_dir = invocation_context_path
            .parent()
            .ok_or_else(|| anyhow!("invalid invocation context path"))?;
        let loopy_dir = invocations_dir
            .parent()
            .ok_or_else(|| anyhow!("invalid invocation context path"))?;
        if invocations_dir.file_name().and_then(|name| name.to_str()) != Some("invocations")
            || loopy_dir.file_name().and_then(|name| name.to_str()) != Some(".loopy")
        {
            bail!(
                "invocation context path must live under <workspace>/.loopy/invocations: {}",
                invocation_context_path.display()
            );
        }
        let workspace_root = loopy_dir
            .parent()
            .ok_or_else(|| anyhow!("invalid invocation context path"))?;
        Self::new(workspace_root)
    }

    pub fn open_loop(&self, request: OpenLoopRequest) -> Result<OpenLoopResponse> {
        ops::r#loop::open_loop(self, request)
    }

    pub fn rebuild_projections(&self) -> Result<()> {
        self.rebuild_all_projections()
    }

    pub fn rebuild_all_projections(&self) -> Result<()> {
        let mut connection = self.open_connection()?;
        let transaction = begin_immediate_transaction(&mut connection)?;
        projection::rebuild_all_projections(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn rebuild_loop_projections(&self, loop_id: &str) -> Result<()> {
        let mut connection = self.open_connection()?;
        let transaction = begin_immediate_transaction(&mut connection)?;
        projection::rebuild_single_loop_projections(&transaction, loop_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn show_loop(&self, request: ShowLoopRequest) -> Result<ShowLoopSummary> {
        query::show_loop(self, request)
    }

    pub fn handoff_to_caller_finalize(
        &self,
        request: HandoffToCallerFinalizeRequest,
    ) -> Result<HandoffToCallerFinalizeResponse> {
        ops::caller_finalize::handoff_to_caller_finalize(self, request)
    }

    pub fn begin_caller_finalize(
        &self,
        request: BeginCallerFinalizeRequest,
    ) -> Result<BeginCallerFinalizeResponse> {
        ops::caller_finalize::begin_caller_finalize(self, request)
    }

    pub fn block_caller_finalize(
        &self,
        request: BlockCallerFinalizeRequest,
    ) -> Result<BlockCallerFinalizeResponse> {
        ops::caller_finalize::block_caller_finalize(self, request)
    }

    pub fn prepare_worktree(
        &self,
        request: PrepareWorktreeRequest,
    ) -> Result<PrepareWorktreeResponse> {
        ops::r#loop::prepare_worktree(self, request)
    }

    pub fn open_review_round(
        &self,
        request: OpenReviewRoundRequest,
    ) -> Result<OpenReviewRoundResponse> {
        ops::r#loop::open_review_round(self, request)
    }

    pub fn start_worker_invocation(
        &self,
        request: StartWorkerInvocationRequest,
    ) -> Result<StartWorkerInvocationResponse> {
        ops::invocation::start_worker_invocation(self, request)
    }

    pub fn start_reviewer_invocation(
        &self,
        request: StartReviewerInvocationRequest,
    ) -> Result<StartReviewerInvocationResponse> {
        ops::invocation::start_reviewer_invocation(self, request)
    }

    pub fn submit_checkpoint_plan(
        &self,
        request: SubmitCheckpointPlanRequest,
    ) -> Result<SubmitCheckpointPlanResponse> {
        ops::submissions::submit_checkpoint_plan(self, request)
    }

    pub fn submit_checkpoint_review(
        &self,
        request: SubmitCheckpointReviewRequest,
    ) -> Result<SubmitCheckpointReviewResponse> {
        ops::submissions::submit_checkpoint_review(self, request)
    }

    pub fn submit_artifact_review(
        &self,
        request: SubmitArtifactReviewRequest,
    ) -> Result<SubmitArtifactReviewResponse> {
        ops::submissions::submit_artifact_review(self, request)
    }

    pub fn request_timeout_extension(
        &self,
        request: RequestTimeoutExtensionRequest,
    ) -> Result<RequestTimeoutExtensionResponse> {
        ops::submissions::request_timeout_extension(self, request)
    }

    pub fn submit_candidate_commit(
        &self,
        request: SubmitCandidateCommitRequest,
    ) -> Result<SubmitCandidateCommitResponse> {
        ops::submissions::submit_candidate_commit(self, request)
    }

    pub fn declare_worker_blocked(
        &self,
        request: DeclareWorkerBlockedRequest,
    ) -> Result<DeclareWorkerBlockedResponse> {
        ops::submissions::declare_worker_blocked(self, request)
    }

    pub fn declare_review_blocked(
        &self,
        request: DeclareReviewBlockedRequest,
    ) -> Result<DeclareReviewBlockedResponse> {
        ops::submissions::declare_review_blocked(self, request)
    }

    pub fn finalize_failure(
        &self,
        request: FinalizeFailureRequest,
    ) -> Result<FinalizeFailureResponse> {
        ops::r#loop::finalize_failure(self, request)
    }

    pub fn finalize_success(
        &self,
        request: FinalizeSuccessRequest,
    ) -> Result<FinalizeSuccessResponse> {
        ops::caller_finalize::finalize_success(self, request)
    }

    fn open_connection(&self) -> Result<Connection> {
        let db_path = self.db_path()?;
        let connection = Connection::open(&db_path)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        configure_write_connection(&connection)?;
        if projection::schema_bootstrap_required(&connection)? {
            projection::bootstrap_schema(&connection)?;
        }
        Ok(connection)
    }

    fn open_read_only_connection(&self) -> Result<Connection> {
        let db_path = self.db_path()?;
        let connection = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        configure_read_only_connection(&connection)?;
        Ok(connection)
    }

    fn db_path(&self) -> Result<PathBuf> {
        self.validate_db_path()?;
        Ok(self
            .db_path_override
            .clone()
            .unwrap_or_else(|| self.expected_db_path()))
    }

    fn expected_db_path(&self) -> PathBuf {
        self.workspace_root.join(".loopy").join("loopy.db")
    }

    fn validate_db_path(&self) -> Result<()> {
        if let Some(override_path) = &self.db_path_override {
            let normalized_override = if override_path.is_absolute() {
                override_path.clone()
            } else {
                self.workspace_root.join(override_path)
            };
            let expected = self.expected_db_path();
            if normalized_override != expected {
                bail!(
                    "authoritative runtime state must live at {} (expected {}, got {})",
                    FIXED_DB_RELATIVE_PATH,
                    expected.display(),
                    normalized_override.display()
                );
            }
        }
        Ok(())
    }
}

pub(crate) fn begin_immediate_transaction(connection: &mut Connection) -> Result<Transaction<'_>> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("failed to begin SQLite immediate transaction")
}

fn configure_write_connection(connection: &Connection) -> Result<()> {
    configure_common_connection_settings(connection)
}

fn configure_read_only_connection(connection: &Connection) -> Result<()> {
    configure_common_connection_settings(connection)
}

fn configure_common_connection_settings(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .context("failed to configure SQLite busy timeout")?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable SQLite foreign key enforcement")?;
    Ok(())
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

#[derive(Debug, Deserialize)]
pub(crate) struct Manifest {
    executors: HashMap<String, ExecutorProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskTypeConfig {
    task_type: String,
    default_planning_worker: String,
    default_artifact_worker: String,
    default_checkpoint_reviewers: Vec<String>,
    default_artifact_reviewers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedRoleSelection {
    task_type: String,
    planning_worker: String,
    artifact_worker: String,
    checkpoint_reviewers: Vec<String>,
    artifact_reviewers: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct NormalizedOpenLoopInput {
    summary: String,
    task_type: String,
    context: String,
    constraints: Value,
    bypass_sandbox: bool,
    resolved_role_selection: ResolvedRoleSelection,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExecutorProfile {
    kind: String,
    command: String,
    args: Vec<String>,
    #[serde(default)]
    bypass_sandbox_args: Option<Vec<String>>,
    #[serde(default)]
    bypass_sandbox_inherit_env: bool,
    cwd: String,
    timeout_sec: i64,
    transcript_capture: String,
    env_allow: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RoleFrontMatter {
    role: String,
    executor: String,
}

#[derive(Debug)]
pub(crate) struct ShowLoopCoreState {
    loop_id: String,
    status: String,
    phase: String,
    updated_at: String,
}

#[derive(Debug)]
pub(crate) struct LoopState {
    phase: String,
    status: String,
    base_commit_sha: String,
    loop_input_ref: String,
    resolved_role_selection_ref: String,
    worktree_path: String,
    worktree_branch: String,
    worktree_label: String,
    failure_summary: Option<String>,
}

pub(crate) struct InvocationDispatchState {
    loop_id: String,
    invocation_context_ref: String,
    executor_config_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewSlotState {
    review_slot_id: String,
    #[serde(default)]
    reviewer_role_id: Option<String>,
    status: String,
    decision: Option<String>,
    submission_content_ref: Option<String>,
}

pub(crate) struct ReviewRoundState {
    review_kind: String,
    target_type: String,
    target_ref: String,
    target_metadata: Value,
    slot_state: Vec<ReviewSlotState>,
}

pub(crate) struct InvocationState {
    loop_id: String,
    invocation_role: String,
    stage: String,
    status: String,
    token: String,
    token_state: String,
    accepted_api: Option<String>,
    accepted_submission_id: Option<String>,
    invocation_context_ref: String,
    allowed_terminal_apis: Vec<String>,
    review_round_id: Option<String>,
    review_slot_id: Option<String>,
}

pub(crate) struct FailureEventState {
    event_id: i64,
    failure_cause_type: String,
    summary: String,
    phase_at_failure: String,
    last_stable_context: Value,
}

pub(crate) struct WorktreeCleanupWarningState {
    summary: String,
    worktree_path: String,
    worktree_branch: String,
    worktree_label: String,
}

pub(crate) struct CheckpointState {
    checkpoint_id: String,
    sequence_index: i64,
    title: String,
    kind: String,
    deliverables: Vec<CheckpointDeliverable>,
    acceptance: CheckpointAcceptance,
    execution_state: String,
    accepted_commit_sha: Option<String>,
    candidate_commit_sha: Option<String>,
}

pub(crate) struct CandidateCommitState {
    checkpoint_id: String,
    title: String,
    commit_sha: String,
    change_summary: Value,
}

pub(crate) struct AcceptedArtifactMaterial {
    title: String,
    change_summary: Value,
}

pub(crate) struct CallerIntegrationState {
    caller_branch: String,
    final_head_sha: String,
    strategy: String,
    landed_commit_shas: Vec<String>,
    resolution_notes: Option<String>,
}

pub(crate) struct AuthenticatedTerminalRequest {
    loop_id: String,
    invocation_id: String,
    invocation_state: InvocationState,
    stored_invocation_context: Value,
}

pub(crate) struct TimeoutExtensionRequestState {
    request_content_ref: String,
    requested_timeout_sec: i64,
    progress_summary: String,
    rationale: String,
}

pub(crate) enum ReviewSlotTerminal {
    Decision {
        decision: String,
        submission_content_ref: String,
    },
    Blocked {
        submission_content_ref: String,
    },
}
