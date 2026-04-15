use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use loopy::{
    BeginCallerFinalizeRequest, BlockCallerFinalizeRequest, CallerIntegrationSummary,
    CheckpointAcceptance, CheckpointDeliverable, CheckpointPlanItem, DeclareReviewBlockedRequest,
    DeclareWorkerBlockedRequest, FinalizeFailureRequest, FinalizeSuccessRequest,
    HandoffToCallerFinalizeRequest, OpenLoopRequest, OpenReviewRoundRequest,
    PrepareWorktreeRequest, RequestTimeoutExtensionRequest, ReviewKind, Runtime, ShowLoopRequest,
    ShowLoopSummary, StartReviewerInvocationRequest, StartWorkerInvocationRequest,
    SubmitArtifactReviewRequest, SubmitCandidateCommitRequest, SubmitCheckpointPlanRequest,
    SubmitCheckpointReviewRequest, WorkerStage,
};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "loopy")]
#[command(about = "Local runtime for loopy:submit-loop")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    OpenLoop {
        #[arg(long)]
        summary: String,
        #[arg(long)]
        task_type: String,
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        planning_worker: Option<String>,
        #[arg(long)]
        artifact_worker: Option<String>,
        #[arg(long)]
        checkpoint_reviewers_json: Option<String>,
        #[arg(long)]
        artifact_reviewers_json: Option<String>,
        #[arg(long)]
        constraints_json: Option<String>,
        #[arg(long, default_value_t = false)]
        bypass_sandbox: bool,
    },
    ShowLoop {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    PrepareWorktree {
        #[arg(long)]
        loop_id: String,
    },
    StartWorkerInvocation {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        stage: CliWorkerStage,
        #[arg(long)]
        checkpoint_id: Option<String>,
    },
    OpenReviewRound {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        review_kind: CliReviewKind,
        #[arg(long)]
        target_type: String,
        #[arg(long)]
        target_ref: String,
    },
    StartReviewerInvocation {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        review_round_id: String,
        #[arg(long)]
        review_slot_id: String,
    },
    SubmitCheckpointPlan {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        submission_id: String,
        #[arg(long)]
        checkpoints_json: String,
        #[arg(long, default_value = "[]")]
        improvement_opportunities_json: String,
        #[arg(long)]
        notes: Option<String>,
    },
    SubmitCheckpointReview {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        submission_id: String,
        #[arg(long)]
        decision: String,
        #[arg(long)]
        blocking_issues_json: Option<String>,
        #[arg(long)]
        nonblocking_issues_json: Option<String>,
        #[arg(long)]
        summary: String,
        #[arg(long, default_value = "[]")]
        improvement_opportunities_json: String,
        #[arg(long)]
        notes: Option<String>,
    },
    SubmitArtifactReview {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        submission_id: String,
        #[arg(long)]
        decision: String,
        #[arg(long)]
        blocking_issues_json: Option<String>,
        #[arg(long)]
        nonblocking_issues_json: Option<String>,
        #[arg(long)]
        summary: String,
        #[arg(long, default_value = "[]")]
        improvement_opportunities_json: String,
        #[arg(long)]
        notes: Option<String>,
    },
    RequestTimeoutExtension {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        requested_timeout_sec: i64,
        #[arg(long)]
        progress_summary: String,
        #[arg(long)]
        rationale: String,
    },
    SubmitCandidateCommit {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        submission_id: String,
        #[arg(long)]
        candidate_commit_sha: String,
        #[arg(long)]
        change_summary_json: String,
        #[arg(long, default_value = "[]")]
        improvement_opportunities_json: String,
        #[arg(long)]
        notes: Option<String>,
    },
    DeclareWorkerBlocked {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        submission_id: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        rationale: String,
        #[arg(long)]
        why_unrecoverable: String,
        #[arg(long)]
        notes: Option<String>,
    },
    DeclareReviewBlocked {
        #[arg(long)]
        invocation_context_path: PathBuf,
        #[arg(long)]
        submission_id: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        rationale: String,
        #[arg(long)]
        why_unrecoverable: String,
        #[arg(long)]
        notes: Option<String>,
    },
    FinalizeFailure {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        failure_cause_type: String,
        #[arg(long)]
        summary: String,
    },
    HandoffToCallerFinalize {
        #[arg(long)]
        loop_id: String,
    },
    BeginCallerFinalize {
        #[arg(long)]
        loop_id: String,
    },
    BlockCallerFinalize {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        strategy_summary: String,
        #[arg(long)]
        blocking_summary: String,
        #[arg(long)]
        human_question: String,
        #[arg(long, default_value = "[]")]
        conflicting_files_json: String,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long, default_value_t = false)]
        has_in_progress_integration: bool,
    },
    FinalizeSuccess {
        #[arg(long)]
        loop_id: String,
        #[arg(long)]
        integration_summary_json: String,
    },
    RebuildProjections {
        #[arg(long)]
        loop_id: Option<String>,
        #[arg(long, default_value_t = false)]
        full: bool,
    },
    MockExecutor {
        actor_role: String,
        invocation_context_path: PathBuf,
    },
    MockSubmitPlanWorker {
        invocation_context_path: PathBuf,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum CliWorkerStage {
    Planning,
    Artifact,
}

#[derive(Debug, Clone, ValueEnum)]
enum CliReviewKind {
    Checkpoint,
    Artifact,
}

impl From<CliWorkerStage> for WorkerStage {
    fn from(value: CliWorkerStage) -> Self {
        match value {
            CliWorkerStage::Planning => WorkerStage::Planning,
            CliWorkerStage::Artifact => WorkerStage::Artifact,
        }
    }
}

impl From<CliReviewKind> for ReviewKind {
    fn from(value: CliReviewKind) -> Self {
        match value {
            CliReviewKind::Checkpoint => ReviewKind::Checkpoint,
            CliReviewKind::Artifact => ReviewKind::Artifact,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let current_workspace_root = std::env::current_dir()?;
    let installed_skill_root = detect_cli_installed_skill_root()?;
    let runtime_for_workspace = |workspace_root: &Path| -> Result<Runtime> {
        if let Some(skill_root) = installed_skill_root.clone() {
            Runtime::with_installed_skill_root(workspace_root, skill_root)
        } else {
            Runtime::new(workspace_root)
        }
    };
    let runtime = runtime_for_workspace(&current_workspace_root)?;

    match cli.command {
        Commands::OpenLoop {
            summary,
            task_type,
            context,
            planning_worker,
            artifact_worker,
            checkpoint_reviewers_json,
            artifact_reviewers_json,
            constraints_json,
            bypass_sandbox,
        } => {
            let skill_root = runtime.installed_skill_root();
            let coordinator_prompt = fs::read_to_string(skill_root.join("coordinator.md"))
                .with_context(|| {
                    format!("failed to read {}/coordinator.md", skill_root.display())
                })?;
            let checkpoint_reviewers = checkpoint_reviewers_json
                .as_deref()
                .map(|json| {
                    serde_json::from_str(json)
                        .context("failed to parse --checkpoint-reviewers-json as JSON")
                })
                .transpose()?;
            let artifact_reviewers = artifact_reviewers_json
                .as_deref()
                .map(|json| {
                    serde_json::from_str(json)
                        .context("failed to parse --artifact-reviewers-json as JSON")
                })
                .transpose()?;
            let constraints = constraints_json
                .as_deref()
                .map(|json| {
                    serde_json::from_str(json).context("failed to parse --constraints-json as JSON")
                })
                .transpose()?;
            let response = runtime.open_loop(OpenLoopRequest {
                summary,
                task_type,
                context,
                planning_worker,
                artifact_worker,
                checkpoint_reviewers,
                artifact_reviewers,
                constraints,
                bypass_sandbox: Some(bypass_sandbox),
                coordinator_prompt,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::ShowLoop {
            loop_id,
            workspace,
            json,
        } => {
            let runtime = runtime_for_workspace(
                workspace
                    .as_deref()
                    .unwrap_or(current_workspace_root.as_path()),
            )?;
            let summary = runtime.show_loop(ShowLoopRequest { loop_id })?;
            if json {
                println!("{}", serde_json::to_string(&summary)?);
            } else {
                print_show_loop_table(&summary);
            }
        }
        Commands::PrepareWorktree { loop_id } => {
            let response = runtime.prepare_worktree(PrepareWorktreeRequest { loop_id })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::StartWorkerInvocation {
            loop_id,
            stage,
            checkpoint_id,
        } => {
            let response = runtime.start_worker_invocation(StartWorkerInvocationRequest {
                loop_id,
                stage: stage.into(),
                checkpoint_id,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::OpenReviewRound {
            loop_id,
            review_kind,
            target_type,
            target_ref,
        } => {
            let response = runtime.open_review_round(OpenReviewRoundRequest {
                loop_id,
                review_kind: review_kind.into(),
                target_type,
                target_ref,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::StartReviewerInvocation {
            loop_id,
            review_round_id,
            review_slot_id,
        } => {
            let response = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
                loop_id,
                review_round_id,
                review_slot_id,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::SubmitCheckpointPlan {
            invocation_context_path,
            submission_id,
            checkpoints_json,
            improvement_opportunities_json,
            notes,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let checkpoints: Vec<CheckpointPlanItem> = serde_json::from_str(&checkpoints_json)
                .context("failed to parse --checkpoints-json as JSON")?;
            let improvement_opportunities = parse_improvement_opportunities(
                &improvement_opportunities_json,
                "--improvement-opportunities-json",
            )?;
            let response = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
                invocation_context_path,
                submission_id,
                checkpoints,
                improvement_opportunities: Some(improvement_opportunities),
                notes,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::SubmitCheckpointReview {
            invocation_context_path,
            submission_id,
            decision,
            blocking_issues_json,
            nonblocking_issues_json,
            summary,
            improvement_opportunities_json,
            notes,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let blocking_issues = parse_review_issue_objects(
                blocking_issues_json.as_deref().unwrap_or("[]"),
                "--blocking-issues-json",
            )?;
            let nonblocking_issues = parse_review_issue_objects(
                nonblocking_issues_json.as_deref().unwrap_or("[]"),
                "--nonblocking-issues-json",
            )?;
            let improvement_opportunities = parse_improvement_opportunities(
                &improvement_opportunities_json,
                "--improvement-opportunities-json",
            )?;
            let response = runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
                invocation_context_path,
                submission_id,
                decision,
                summary,
                blocking_issues,
                nonblocking_issues: Some(nonblocking_issues),
                improvement_opportunities: Some(improvement_opportunities),
                notes,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::SubmitArtifactReview {
            invocation_context_path,
            submission_id,
            decision,
            blocking_issues_json,
            nonblocking_issues_json,
            summary,
            improvement_opportunities_json,
            notes,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let blocking_issues = parse_review_issue_objects(
                blocking_issues_json.as_deref().unwrap_or("[]"),
                "--blocking-issues-json",
            )?;
            let nonblocking_issues = parse_review_issue_objects(
                nonblocking_issues_json.as_deref().unwrap_or("[]"),
                "--nonblocking-issues-json",
            )?;
            let improvement_opportunities = parse_improvement_opportunities(
                &improvement_opportunities_json,
                "--improvement-opportunities-json",
            )?;
            let response = runtime.submit_artifact_review(SubmitArtifactReviewRequest {
                invocation_context_path,
                submission_id,
                decision,
                summary,
                blocking_issues,
                nonblocking_issues: Some(nonblocking_issues),
                improvement_opportunities: Some(improvement_opportunities),
                notes,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::RequestTimeoutExtension {
            invocation_context_path,
            requested_timeout_sec,
            progress_summary,
            rationale,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let response = runtime.request_timeout_extension(RequestTimeoutExtensionRequest {
                invocation_context_path,
                requested_timeout_sec,
                progress_summary,
                rationale,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::SubmitCandidateCommit {
            invocation_context_path,
            submission_id,
            candidate_commit_sha,
            change_summary_json,
            improvement_opportunities_json,
            notes,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let change_summary: Value = serde_json::from_str(&change_summary_json)
                .context("failed to parse --change-summary-json as JSON")?;
            let improvement_opportunities = parse_improvement_opportunities(
                &improvement_opportunities_json,
                "--improvement-opportunities-json",
            )?;
            let response = runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
                invocation_context_path,
                submission_id,
                candidate_commit_sha,
                change_summary,
                improvement_opportunities: Some(improvement_opportunities),
                notes,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::DeclareWorkerBlocked {
            invocation_context_path,
            submission_id,
            summary,
            rationale,
            why_unrecoverable,
            notes,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let response = runtime.declare_worker_blocked(DeclareWorkerBlockedRequest {
                invocation_context_path,
                submission_id,
                summary,
                rationale,
                why_unrecoverable,
                notes,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::DeclareReviewBlocked {
            invocation_context_path,
            submission_id,
            summary,
            rationale,
            why_unrecoverable,
            notes,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let response = runtime.declare_review_blocked(DeclareReviewBlockedRequest {
                invocation_context_path,
                submission_id,
                summary,
                rationale,
                why_unrecoverable,
                notes,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::FinalizeFailure {
            loop_id,
            failure_cause_type,
            summary,
        } => {
            let response = runtime.finalize_failure(FinalizeFailureRequest {
                loop_id,
                failure_cause_type,
                summary,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::HandoffToCallerFinalize { loop_id } => {
            let response =
                runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest { loop_id })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::BeginCallerFinalize { loop_id } => {
            let response = runtime.begin_caller_finalize(BeginCallerFinalizeRequest { loop_id })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::BlockCallerFinalize {
            loop_id,
            strategy_summary,
            blocking_summary,
            human_question,
            conflicting_files_json,
            notes,
            has_in_progress_integration,
        } => {
            let conflicting_files: Vec<String> = serde_json::from_str(&conflicting_files_json)
                .context("failed to parse --conflicting-files-json")?;
            let response = runtime.block_caller_finalize(BlockCallerFinalizeRequest {
                loop_id,
                strategy_summary,
                blocking_summary,
                human_question,
                conflicting_files,
                notes,
                has_in_progress_integration,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::FinalizeSuccess {
            loop_id,
            integration_summary_json,
        } => {
            let integration_summary: CallerIntegrationSummary =
                serde_json::from_str(&integration_summary_json)
                    .context("failed to parse --integration-summary-json")?;
            let response = runtime.finalize_success(FinalizeSuccessRequest {
                loop_id,
                integration_summary,
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Commands::RebuildProjections { loop_id, full } => {
            if full {
                if loop_id.is_some() {
                    anyhow::bail!("--loop-id cannot be combined with --full");
                }
                runtime.rebuild_all_projections()?;
            } else {
                let loop_id = loop_id
                    .context("rebuild-projections requires --loop-id unless --full is set")?;
                runtime.rebuild_loop_projections(&loop_id)?;
            }
            println!("{}", serde_json::json!({"status": "ok"}));
        }
        Commands::MockExecutor {
            actor_role,
            invocation_context_path,
        } => {
            let mut stdin_payload = String::new();
            std::io::stdin().read_to_string(&mut stdin_payload)?;
            let payload = serde_json::json!({
                "status": "mock",
                "actor_role": actor_role,
                "invocation_context_path": invocation_context_path,
                "stdin_payload": stdin_payload,
            });
            println!("{}", serde_json::to_string(&payload)?);
        }
        Commands::MockSubmitPlanWorker {
            invocation_context_path,
        } => {
            let runtime = Runtime::from_invocation_context_path(&invocation_context_path)?;
            let response = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
                invocation_context_path,
                submission_id: "mock-submit-plan-worker".to_owned(),
                checkpoints: vec![mock_checkpoint_plan_item()],
                improvement_opportunities: None,
                notes: Some("Deterministic mock planning submission".to_owned()),
            })?;
            println!("{}", serde_json::to_string(&response)?);
        }
    }

    Ok(())
}

fn mock_checkpoint_plan_item() -> CheckpointPlanItem {
    CheckpointPlanItem {
        title: "Mock checkpoint".to_owned(),
        kind: "artifact".to_owned(),
        deliverables: vec![CheckpointDeliverable {
            path: "mock-artifacts/mock-checkpoint.txt".to_owned(),
            deliverable_type: "file".to_owned(),
        }],
        acceptance: CheckpointAcceptance {
            verification_steps: vec!["test -f mock-artifacts/mock-checkpoint.txt".to_owned()],
            expected_outcomes: vec!["Mock checkpoint artifact exists".to_owned()],
        },
    }
}

fn print_show_loop_table(summary: &ShowLoopSummary) {
    let mut rows = vec![
        ("loop_id".to_owned(), summary.loop_id.clone()),
        ("status".to_owned(), summary.status.clone()),
        ("phase".to_owned(), summary.phase.clone()),
        ("updated_at".to_owned(), summary.updated_at.clone()),
        (
            "bypass_sandbox".to_owned(),
            summary.bypass_sandbox.to_string(),
        ),
    ];

    rows.push((
        "caller_finalize.status".to_owned(),
        summary
            .caller_finalize
            .as_ref()
            .map(|caller_finalize| caller_finalize.status.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "caller_finalize.blocking_summary".to_owned(),
        summary
            .caller_finalize
            .as_ref()
            .and_then(|caller_finalize| caller_finalize.blocking_summary.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "caller_finalize.human_question".to_owned(),
        summary
            .caller_finalize
            .as_ref()
            .and_then(|caller_finalize| caller_finalize.human_question.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "plan.latest_submitted".to_owned(),
        summary
            .plan
            .as_ref()
            .and_then(|plan| {
                plan.latest_submitted_plan_revision
                    .map(|value| value.to_string())
            })
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "plan.executable".to_owned(),
        summary
            .plan
            .as_ref()
            .and_then(|plan| {
                plan.current_executable_plan_revision
                    .map(|value| value.to_string())
            })
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "worktree.label".to_owned(),
        summary
            .worktree
            .as_ref()
            .map(|worktree| worktree.label.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "worktree.branch".to_owned(),
        summary
            .worktree
            .as_ref()
            .map(|worktree| worktree.branch.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "worktree.lifecycle".to_owned(),
        summary
            .worktree
            .as_ref()
            .map(|worktree| worktree.lifecycle.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_invocation.role".to_owned(),
        summary
            .latest_invocation
            .as_ref()
            .map(|invocation| invocation.invocation_role.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_invocation.stage".to_owned(),
        summary
            .latest_invocation
            .as_ref()
            .map(|invocation| invocation.stage.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_invocation.status".to_owned(),
        summary
            .latest_invocation
            .as_ref()
            .map(|invocation| invocation.status.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_invocation.updated_at".to_owned(),
        summary
            .latest_invocation
            .as_ref()
            .map(|invocation| invocation.updated_at.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_review.kind".to_owned(),
        summary
            .latest_review
            .as_ref()
            .map(|review| review.review_kind.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_review.status".to_owned(),
        summary
            .latest_review
            .as_ref()
            .map(|review| review.round_status.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_review.target_type".to_owned(),
        summary
            .latest_review
            .as_ref()
            .map(|review| review.target_type.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "latest_review.target_ref".to_owned(),
        summary
            .latest_review
            .as_ref()
            .map(|review| review.target_ref.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "result.status".to_owned(),
        summary
            .result
            .as_ref()
            .map(|result| result.status.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));
    rows.push((
        "result.generated_at".to_owned(),
        summary
            .result
            .as_ref()
            .map(|result| result.generated_at.clone())
            .unwrap_or_else(|| "-".to_owned()),
    ));

    let field_width = rows.iter().map(|(field, _)| field.len()).max().unwrap_or(5);
    println!("{:<width$}  value", "field", width = field_width);
    println!("{:-<width$}  {:-<5}", "", "", width = field_width);
    for (field, value) in rows {
        println!("{:<width$}  {}", field, value, width = field_width);
    }
}

fn parse_review_issue_objects(json_text: &str, flag_name: &str) -> Result<Vec<Value>> {
    let values: Vec<Value> = serde_json::from_str(json_text)
        .with_context(|| format!("failed to parse {flag_name} as JSON"))?;
    values
        .into_iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow!("{flag_name} entries must be JSON objects"))?;
            for key in ["summary", "rationale", "expected_revision"] {
                let valid = object
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_some();
                if !valid {
                    return Err(anyhow!(
                        "{flag_name} entries must include non-empty string field {key}"
                    ));
                }
            }
            Ok(Value::Object(object.clone()))
        })
        .collect()
}

fn parse_improvement_opportunities(json_text: &str, flag_name: &str) -> Result<Vec<Value>> {
    let values: Vec<Value> = serde_json::from_str(json_text)
        .with_context(|| format!("failed to parse {flag_name} as JSON"))?;
    values
        .into_iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow!("{flag_name} entries must be JSON objects"))?;
            for key in ["summary", "rationale", "suggested_follow_up"] {
                let valid = object
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_some();
                if !valid {
                    return Err(anyhow!(
                        "{flag_name} entries must include non-empty string field {key}"
                    ));
                }
            }
            Ok(Value::Object(object.clone()))
        })
        .collect()
}

fn detect_cli_installed_skill_root() -> Result<Option<PathBuf>> {
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let Some(bin_dir) = current_exe.parent() else {
        return Ok(None);
    };
    let Some(bundle_root) = bin_dir.parent() else {
        return Ok(None);
    };
    if bundle_root.join("submit-loop.toml").is_file()
        && bundle_root.join("coordinator.md").is_file()
    {
        return Ok(Some(bundle_root.to_path_buf()));
    }
    Ok(None)
}
