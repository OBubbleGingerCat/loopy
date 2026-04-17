// Public runtime API surface lives here; internal helpers and state machines stay in other modules.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenLoopRequest {
    pub summary: String,
    pub task_type: String,
    pub context: Option<String>,
    pub planning_worker: Option<String>,
    pub artifact_worker: Option<String>,
    pub checkpoint_reviewers: Option<Vec<String>>,
    pub artifact_reviewers: Option<Vec<String>>,
    pub constraints: Option<Value>,
    #[serde(default)]
    pub bypass_sandbox: Option<bool>,
    pub coordinator_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenLoopResponse {
    pub loop_id: String,
    pub branch: String,
    pub label: String,
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopRequest {
    pub loop_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopCallerFinalizeSummary {
    pub status: String,
    pub updated_at: String,
    pub blocking_summary: Option<String>,
    pub human_question: Option<String>,
    #[serde(default)]
    pub conflicting_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopSummary {
    pub loop_id: String,
    pub status: String,
    pub phase: String,
    pub updated_at: String,
    #[serde(default)]
    pub bypass_sandbox: bool,
    pub caller_finalize: Option<ShowLoopCallerFinalizeSummary>,
    pub plan: Option<ShowLoopPlanSummary>,
    pub worktree: Option<ShowLoopWorktreeSummary>,
    pub latest_invocation: Option<ShowLoopInvocationSummary>,
    pub latest_review: Option<ShowLoopReviewSummary>,
    pub result: Option<ShowLoopResultSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopPlanSummary {
    pub latest_submitted_plan_revision: Option<i64>,
    pub current_executable_plan_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopWorktreeSummary {
    pub path: String,
    pub branch: String,
    pub label: String,
    pub lifecycle: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopInvocationSummary {
    pub invocation_id: String,
    pub invocation_role: String,
    pub stage: String,
    pub status: String,
    pub accepted_api: Option<String>,
    pub review_round_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopReviewSummary {
    pub review_round_id: String,
    pub review_kind: String,
    pub round_status: String,
    pub target_type: String,
    pub target_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowLoopResultSummary {
    pub status: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerFinalizeWorktreeRef {
    pub path: String,
    pub branch: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerFinalizeArtifactSummary {
    pub checkpoint_id: String,
    pub checkpoint_title: String,
    pub accepted_commit_sha: String,
    pub change_summary: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerFacingImprovementSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerFacingImprovementSummary {
    pub source: CallerFacingImprovementSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<String>,
    #[serde(default)]
    pub improvement_opportunities: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffToCallerFinalizeRequest {
    pub loop_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffToCallerFinalizeResponse {
    pub loop_id: String,
    pub phase: String,
    pub task_summary: String,
    pub worktree_ref: CallerFinalizeWorktreeRef,
    pub artifact_summary: Vec<CallerFinalizeArtifactSummary>,
    #[serde(default)]
    pub improvement_opportunities: Vec<CallerFacingImprovementSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginCallerFinalizeRequest {
    pub loop_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginCallerFinalizeResponse {
    pub loop_id: String,
    pub phase: String,
    pub task_summary: String,
    pub worktree_ref: CallerFinalizeWorktreeRef,
    pub artifact_summary: Vec<CallerFinalizeArtifactSummary>,
    #[serde(default)]
    pub improvement_opportunities: Vec<CallerFacingImprovementSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockCallerFinalizeRequest {
    pub loop_id: String,
    pub strategy_summary: String,
    pub blocking_summary: String,
    pub human_question: String,
    #[serde(default)]
    pub conflicting_files: Vec<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub has_in_progress_integration: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockCallerFinalizeResponse {
    pub loop_id: String,
    pub phase: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareWorktreeRequest {
    pub loop_id: String,
}

pub type PrepareWorktreeResponse = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStage {
    Planning,
    Artifact,
}

impl WorkerStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Artifact => "artifact",
        }
    }

    pub fn allowed_terminal_apis(&self) -> Vec<&'static str> {
        match self {
            Self::Planning => vec![
                "SUBMIT_LOOP__submit_checkpoint_plan",
                "SUBMIT_LOOP__declare_worker_blocked",
            ],
            Self::Artifact => vec![
                "SUBMIT_LOOP__submit_candidate_commit",
                "SUBMIT_LOOP__declare_worker_blocked",
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    Checkpoint,
    Artifact,
}

impl ReviewKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::Artifact => "artifact",
        }
    }

    pub fn stage(&self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint_review",
            Self::Artifact => "artifact_review",
        }
    }

    pub fn allowed_terminal_apis(&self) -> Vec<&'static str> {
        match self {
            Self::Checkpoint => vec![
                "SUBMIT_LOOP__submit_checkpoint_review",
                "SUBMIT_LOOP__declare_review_blocked",
            ],
            Self::Artifact => vec![
                "SUBMIT_LOOP__submit_artifact_review",
                "SUBMIT_LOOP__declare_review_blocked",
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartWorkerInvocationRequest {
    pub loop_id: String,
    pub stage: WorkerStage,
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartReviewerInvocationRequest {
    pub loop_id: String,
    pub review_round_id: String,
    pub review_slot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartInvocationResponse {
    pub invocation_id: String,
    pub token: String,
    pub role_definition_ref: String,
    pub executor_config_ref: String,
    pub invocation_context_ref: String,
    pub accepted_terminal_api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_result: Option<Value>,
    pub transcript_segment_count: usize,
}

pub type StartWorkerInvocationResponse = StartInvocationResponse;
pub type StartReviewerInvocationResponse = StartInvocationResponse;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenReviewRoundRequest {
    pub loop_id: String,
    pub review_kind: ReviewKind,
    pub target_type: String,
    pub target_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenReviewRoundResponse {
    pub review_round_id: String,
    pub review_slot_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointDeliverable {
    pub path: String,
    #[serde(rename = "type")]
    pub deliverable_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointAcceptance {
    pub verification_steps: Vec<String>,
    pub expected_outcomes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointPlanItem {
    pub title: String,
    pub kind: String,
    pub deliverables: Vec<CheckpointDeliverable>,
    pub acceptance: CheckpointAcceptance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitCheckpointPlanRequest {
    pub invocation_context_path: PathBuf,
    pub submission_id: String,
    pub checkpoints: Vec<CheckpointPlanItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improvement_opportunities: Option<Vec<Value>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitCheckpointPlanResponse {
    pub loop_id: String,
    pub invocation_id: String,
    pub submission_id: String,
    pub accepted_api: String,
    pub plan_revision: i64,
    pub idempotent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSubmissionResponse {
    pub loop_id: String,
    pub invocation_id: String,
    pub submission_id: String,
    pub accepted_api: String,
    pub idempotent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitCheckpointReviewRequest {
    pub invocation_context_path: PathBuf,
    pub submission_id: String,
    pub decision: String,
    pub summary: String,
    pub blocking_issues: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonblocking_issues: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improvement_opportunities: Option<Vec<Value>>,
    pub notes: Option<String>,
}

pub type SubmitCheckpointReviewResponse = TerminalSubmissionResponse;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitArtifactReviewRequest {
    pub invocation_context_path: PathBuf,
    pub submission_id: String,
    pub decision: String,
    pub summary: String,
    pub blocking_issues: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonblocking_issues: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improvement_opportunities: Option<Vec<Value>>,
    pub notes: Option<String>,
}

pub type SubmitArtifactReviewResponse = TerminalSubmissionResponse;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTimeoutExtensionRequest {
    pub invocation_context_path: PathBuf,
    pub requested_timeout_sec: i64,
    pub progress_summary: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTimeoutExtensionResponse {
    pub loop_id: String,
    pub invocation_id: String,
    pub requested_timeout_sec: i64,
    pub progress_summary: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitCandidateCommitRequest {
    pub invocation_context_path: PathBuf,
    pub submission_id: String,
    pub candidate_commit_sha: String,
    pub change_summary: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improvement_opportunities: Option<Vec<Value>>,
    pub notes: Option<String>,
}

pub type SubmitCandidateCommitResponse = TerminalSubmissionResponse;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclareWorkerBlockedRequest {
    pub invocation_context_path: PathBuf,
    pub submission_id: String,
    pub summary: String,
    pub rationale: String,
    pub why_unrecoverable: String,
    pub notes: Option<String>,
}

pub type DeclareWorkerBlockedResponse = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclareReviewBlockedRequest {
    pub invocation_context_path: PathBuf,
    pub submission_id: String,
    pub summary: String,
    pub rationale: String,
    pub why_unrecoverable: String,
    pub notes: Option<String>,
}

pub type DeclareReviewBlockedResponse = TerminalSubmissionResponse;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizeFailureRequest {
    pub loop_id: String,
    pub failure_cause_type: String,
    pub summary: String,
}

pub type FinalizeFailureResponse = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizeSuccessRequest {
    pub loop_id: String,
    pub integration_summary: CallerIntegrationSummary,
}

pub type FinalizeSuccessResponse = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerIntegrationSummary {
    pub strategy: String,
    pub landed_commit_shas: Vec<String>,
    pub resolution_notes: Option<String>,
}
