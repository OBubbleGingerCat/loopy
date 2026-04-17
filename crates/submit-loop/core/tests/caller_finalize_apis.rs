mod support;

use anyhow::{Context, Result};
use loopy::{
    BeginCallerFinalizeRequest, BlockCallerFinalizeRequest, CallerIntegrationSummary,
    FinalizeFailureRequest, FinalizeSuccessRequest, HandoffToCallerFinalizeRequest,
    OpenLoopRequest, OpenReviewRoundRequest, PrepareWorktreeRequest, ReviewKind, Runtime,
    ShowLoopRequest, StartReviewerInvocationRequest, StartWorkerInvocationRequest,
    SubmitArtifactReviewRequest, SubmitCandidateCommitRequest, SubmitCheckpointPlanRequest,
    SubmitCheckpointReviewRequest, WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::{
    accept_single_checkpoint_loop, checkpoint, git, git_output, git_workspace,
    inject_pending_review_round_opened_event, install_bundle_into_workspace,
    invocation_context_path,
};

#[test]
fn handoff_to_caller_finalize_marks_loop_ready_without_moving_head() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize handoff",
        "artifacts/feature.txt",
        "implemented\n",
    )?;
    let head_before = git_output(workspace.path(), &["rev-parse", "HEAD"])?;

    let response = runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    assert_eq!(response.loop_id, accepted.loop_id);
    assert_eq!(response.phase, "ready_for_caller_finalize");
    assert_eq!(response.task_summary, "caller finalize handoff");
    assert_eq!(response.worktree_ref.branch, accepted.branch);
    assert_eq!(response.worktree_ref.label, accepted.label);
    assert_eq!(response.artifact_summary.len(), 1);
    assert_eq!(
        response.artifact_summary[0].accepted_commit_sha,
        accepted.accepted_commit_sha
    );
    assert_eq!(
        git_output(workspace.path(), &["rev-parse", "HEAD"])?,
        head_before
    );

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id,
    })?;
    assert_eq!(show.phase, "ready_for_caller_finalize");
    assert_eq!(
        show.caller_finalize
            .as_ref()
            .map(|summary| summary.status.as_str()),
        Some("ready")
    );

    Ok(())
}

#[test]
fn cannot_hand_off_after_accepted_commits_were_already_integrated() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize after integration",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let next_loop_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(loop_seq), 0) + 1 FROM CORE__events WHERE loop_id = ?1",
        params![accepted.loop_id.clone()],
        |row| row.get(0),
    )?;
    conn.execute(
        r#"
        INSERT INTO CORE__events (
            loop_id,
            loop_seq,
            event_name,
            payload_json,
            occurred_at,
            recorded_at
        ) VALUES (?1, ?2, 'SUBMIT_LOOP__accepted_commits_integrated', ?3, ?4, ?4)
        "#,
        params![
            accepted.loop_id.clone(),
            next_loop_seq,
            json!({
                "phase": "cleanup",
                "caller_branch": git_output(workspace.path(), &["branch", "--show-current"])?,
                "integrated_through_commit_sha": accepted.accepted_commit_sha,
                "accepted_commit_shas": [accepted.accepted_commit_sha],
            })
            .to_string(),
            "2026-04-13T00:00:00Z",
        ],
    )?;
    runtime.rebuild_loop_projections(&accepted.loop_id)?;

    let error = runtime
        .handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
            loop_id: accepted.loop_id.clone(),
        })
        .expect_err("expected handoff after old-style integration to be rejected");
    assert!(
        error.to_string().contains("integrated"),
        "unexpected error: {error:#}"
    );

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id,
    })?;
    assert_eq!(show.phase, "cleanup");
    assert!(show.caller_finalize.is_none());

    Ok(())
}

#[test]
fn cannot_start_artifact_worker_after_caller_finalize_handoff() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize worker fence",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: accepted.loop_id,
            stage: WorkerStage::Artifact,
            checkpoint_id: Some(accepted.checkpoint_id),
        })
        .expect_err("expected caller-finalize handoff to fence off artifact worker restarts");
    assert!(
        error.to_string().contains("caller finalize"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn cannot_open_review_round_after_caller_finalize_handoff() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize blocks review round open",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let error = runtime
        .open_review_round(OpenReviewRoundRequest {
            loop_id: accepted.loop_id,
            review_kind: ReviewKind::Checkpoint,
            target_type: "plan_revision".to_owned(),
            target_ref: "plan-1".to_owned(),
        })
        .expect_err("expected review round opening to be rejected after caller finalize handoff");
    assert!(
        error.to_string().contains("caller finalize"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn cannot_start_reviewer_invocation_after_caller_finalize_handoff() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize blocks reviewer starts",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    let (review_round_id, review_slot_id) = inject_pending_review_round_opened_event(
        workspace.path(),
        &accepted.loop_id,
        "checkpoint",
        "mock",
    )?;

    let error = runtime
        .start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: accepted.loop_id,
            review_round_id,
            review_slot_id,
        })
        .expect_err(
            "expected reviewer invocation opening to be rejected after caller finalize handoff",
        );
    assert!(
        error.to_string().contains("caller finalize"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn stale_reviewer_token_cannot_submit_after_caller_finalize_handoff() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize fences stale reviewer tokens",
        "artifacts/feature.txt",
        "implemented\n",
    )?;
    let (review_round_id, review_slot_id) = inject_pending_review_round_opened_event(
        workspace.path(),
        &accepted.loop_id,
        "checkpoint",
        "mock",
    )?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: accepted.loop_id.clone(),
        review_round_id,
        review_slot_id,
    })?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let error = runtime
        .submit_checkpoint_review(SubmitCheckpointReviewRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &reviewer.invocation_id,
            ),
            submission_id: "stale-review-submit".to_owned(),
            decision: "approve".to_owned(),
            blocking_issues: vec![],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "approved".to_owned(),
            notes: None,
        })
        .expect_err("expected stale reviewer token submission to be rejected after handoff");
    assert!(
        error.to_string().contains("caller finalize"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn stale_worker_token_cannot_submit_after_caller_finalize_handoff() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize fences stale worker tokens",
        "artifacts/feature.txt",
        "implemented\n",
    )?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: accepted.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(accepted.checkpoint_id.clone()),
    })?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let error = runtime
        .submit_candidate_commit(SubmitCandidateCommitRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &worker.invocation_id,
            ),
            submission_id: "stale-worker-submit".to_owned(),
            candidate_commit_sha: accepted.accepted_commit_sha,
            change_summary: json!({
                "headline": "stale submission",
                "files": ["artifacts/feature.txt"],
            }),
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected stale worker token submission to be rejected after handoff");
    assert!(
        error.to_string().contains("caller finalize"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn terminal_failure_clears_caller_finalize_summary() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize summary clears on failure",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: accepted.loop_id.clone(),
        failure_cause_type: "test_failure".to_owned(),
        summary: "forced failure after handoff".to_owned(),
    })?;

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id,
    })?;
    assert_eq!(show.status, "failed");
    assert!(show.caller_finalize.is_none());
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let caller_finalize_rows: i64 = connection.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__caller_finalize_current WHERE loop_id = ?1",
        params![show.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(caller_finalize_rows, 0);

    Ok(())
}

#[test]
fn cannot_prepare_worktree_after_caller_finalize_handoff() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller finalize blocks worktree prepare",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let error = runtime
        .prepare_worktree(PrepareWorktreeRequest {
            loop_id: accepted.loop_id,
        })
        .expect_err("expected prepare_worktree to be rejected after caller finalize handoff");
    assert!(
        error.to_string().contains("caller finalize"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn block_caller_finalize_persists_question_and_begin_resumes_from_blocked() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller blocked handoff",
        "artifacts/conflict.txt",
        "loop version\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let blocked = runtime.block_caller_finalize(BlockCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
        strategy_summary: "Attempted cherry-pick onto main".to_owned(),
        blocking_summary: "Cherry-pick conflicted on README.md".to_owned(),
        human_question: "Keep the main branch header or the loop branch header?".to_owned(),
        conflicting_files: vec!["README.md".to_owned()],
        notes: Some("Auto-resolution could not preserve both headers safely".to_owned()),
        has_in_progress_integration: true,
    })?;

    assert_eq!(blocked.phase, "caller_blocked_on_human");
    assert_eq!(blocked.status, "blocked");

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    let caller_finalize = show
        .caller_finalize
        .context("missing caller finalize summary after block")?;
    assert_eq!(show.phase, "caller_blocked_on_human");
    assert_eq!(caller_finalize.status, "blocked");
    assert_eq!(
        caller_finalize.blocking_summary.as_deref(),
        Some("Cherry-pick conflicted on README.md")
    );
    assert_eq!(
        caller_finalize.human_question.as_deref(),
        Some("Keep the main branch header or the loop branch header?")
    );
    assert_eq!(caller_finalize.conflicting_files, vec!["README.md"]);
    let caller_branch = git_output(workspace.path(), &["branch", "--show-current"])?;
    let caller_head_sha = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let block_context_payload: String = Connection::open(workspace.path().join(".loopy/loopy.db"))?
        .query_row(
            r#"
            SELECT c.payload_json
            FROM SUBMIT_LOOP__caller_finalize_current AS f
            JOIN CORE__contents AS c ON c.content_ref = f.block_context_ref
            WHERE f.loop_id = ?1
            "#,
            params![accepted.loop_id.clone()],
            |row| row.get(0),
        )?;
    let block_context: Value = serde_json::from_str(&block_context_payload)?;
    assert_eq!(
        block_context.get("caller_branch").and_then(Value::as_str),
        Some(caller_branch.as_str())
    );
    assert_eq!(
        block_context.get("caller_head_sha").and_then(Value::as_str),
        Some(caller_head_sha.as_str())
    );
    assert_eq!(
        block_context.get("task_summary").and_then(Value::as_str),
        Some("caller blocked handoff")
    );
    assert_eq!(
        block_context
            .get("strategy_summary")
            .and_then(Value::as_str),
        Some("Attempted cherry-pick onto main")
    );
    assert_eq!(
        block_context
            .get("has_in_progress_integration")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        block_context
            .get("worktree_ref")
            .and_then(|value| value.get("branch"))
            .and_then(Value::as_str),
        Some(accepted.branch.as_str())
    );
    assert_eq!(
        block_context
            .get("worktree_ref")
            .and_then(|value| value.get("label"))
            .and_then(Value::as_str),
        Some(accepted.label.as_str())
    );

    let resumed = runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    assert_eq!(resumed.phase, "caller_finalizing");
    let resumed_show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id,
    })?;
    let resumed_caller_finalize = resumed_show
        .caller_finalize
        .context("missing caller finalize summary after resume")?;
    assert_eq!(resumed_show.phase, "caller_finalizing");
    assert_eq!(resumed_caller_finalize.status, "active");
    assert!(resumed_caller_finalize.blocking_summary.is_none());
    assert!(resumed_caller_finalize.human_question.is_none());
    assert!(resumed_caller_finalize.conflicting_files.is_empty());

    Ok(())
}

#[test]
fn caller_owned_finalize_success_records_integration_summary_and_removes_worktree() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "caller final success",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    let handoff = runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    git(
        workspace.path(),
        &["cherry-pick", &accepted.accepted_commit_sha],
    )?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let caller_branch = git_output(workspace.path(), &["branch", "--show-current"])?;

    let result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: accepted.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head.clone()],
            resolution_notes: Some("Replayed accepted loop output onto main".to_owned()),
        },
    })?;

    assert_eq!(result["status"], Value::String("success".to_owned()));
    assert_eq!(
        result["artifact_summary"][0]["accepted_commit_sha"],
        Value::String(accepted.accepted_commit_sha)
    );
    assert_eq!(
        result["integration_summary"]["strategy"],
        Value::String("cherry_pick".to_owned())
    );
    assert_eq!(
        result["integration_summary"]["caller_branch"],
        Value::String(caller_branch)
    );
    assert_eq!(
        result["integration_summary"]["final_head_sha"],
        Value::String(landed_head.clone())
    );
    assert_eq!(
        result["integration_summary"]["landed_commit_shas"],
        json!([landed_head])
    );
    assert!(result.get("worktree_ref").is_none());
    assert!(!std::path::Path::new(&handoff.worktree_ref.path).try_exists()?);

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: handoff.loop_id,
    })?;
    assert_eq!(show.status, "succeeded");
    assert_eq!(show.phase, "completed");

    Ok(())
}

#[test]
fn caller_finalize_surfaces_latest_improvement_opportunities_per_source() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "caller improvement aggregation".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("surface latest caller-facing improvements per source".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["codex_scope".to_owned()]),
        artifact_reviewers: Some(vec!["codex_checkpoint_contract".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worktree_path = std::path::PathBuf::from(
        prepared["path"]
            .as_str()
            .context("prepare_worktree response missing path")?,
    );

    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &planning_worker.invocation_id,
        ),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Feature")],
        improvement_opportunities: Some(vec![json!({
            "summary": "Document a planner-side checkpoint sizing heuristic",
            "rationale": "The planning worker had to reason from scratch about how small this checkpoint should be.",
            "suggested_follow_up": "Document one checkpoint sizing heuristic in the planner role prompt.",
        })]),
        notes: Some("single checkpoint".to_owned()),
    })?;

    let checkpoint_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let checkpoint_reviewer =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: checkpoint_review_round.review_round_id,
            review_slot_id: checkpoint_review_round.review_slot_ids[0].clone(),
        })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &checkpoint_reviewer.invocation_id,
        ),
        submission_id: "checkpoint-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: Some(vec![json!({
            "summary": "Add a planning rubric example",
            "rationale": "The checkpoint contract is fine, but future planners would benefit from an explicit example.",
            "suggested_follow_up": "Add one canonical planning rubric example to the planner prompt.",
        })]),
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let deliverable = worktree_path.join("artifacts/feature.txt");
    if let Some(parent) = deliverable.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&deliverable, "candidate one\n")?;
    git(&worktree_path, &["add", "artifacts/feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate one"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: git_output(&worktree_path, &["rev-parse", "HEAD"])?,
        change_summary: json!({
            "headline": "candidate one",
            "files": ["artifacts/feature.txt"],
        }),
        improvement_opportunities: Some(vec![json!({
            "summary": "Add a reusable implementation verification script",
            "rationale": "The artifact worker had to reconstruct the same local verification sequence manually.",
            "suggested_follow_up": "Add a reusable implementation verification script for artifact checkpoints.",
        })]),
        notes: Some("first candidate".to_owned()),
    })?;

    let first_artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let first_artifact_reviewer =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: first_artifact_review_round.review_round_id,
            review_slot_id: first_artifact_review_round.review_slot_ids[0].clone(),
        })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &first_artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-reject".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Use the final content",
            "rationale": "The first candidate still uses placeholder text.",
            "expected_revision": "Replace the placeholder text with the finalized deliverable content.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: Some(vec![json!({
            "summary": "Add a reusable artifact acceptance checklist",
            "rationale": "This round exposed a useful future checklist idea.",
            "suggested_follow_up": "Document a reusable artifact acceptance checklist for future loops.",
        })]),
        summary: "reject placeholder".to_owned(),
        notes: Some("replace placeholder text".to_owned()),
    })?;

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    std::fs::write(&deliverable, "candidate two\n")?;
    git(&worktree_path, &["add", "artifacts/feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "--amend", "-m", "candidate two"],
    )?;
    let accepted_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &reopened_worker.invocation_id,
        ),
        submission_id: "candidate-2".to_owned(),
        candidate_commit_sha: accepted_commit_sha.clone(),
        change_summary: json!({
            "headline": "candidate two",
            "files": ["artifacts/feature.txt"],
        }),
        improvement_opportunities: Some(vec![
            json!({
                "summary": "Emit a concise artifact verification report",
                "rationale": "The worker had the evidence, but the caller would benefit from a compact report artifact.",
                "suggested_follow_up": "Emit a concise artifact verification report alongside accepted candidates.",
            }),
            json!({
                "summary": "Emit a concise artifact verification report",
                "rationale": "The worker had the evidence, but the caller would benefit from a compact report artifact.",
                "suggested_follow_up": "Emit a concise artifact verification report alongside accepted candidates.",
            }),
        ]),
        notes: Some("second candidate".to_owned()),
    })?;

    let second_artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let second_artifact_reviewer =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: second_artifact_review_round.review_round_id,
            review_slot_id: second_artifact_review_round.review_slot_ids[0].clone(),
        })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &second_artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: Some(vec![
            json!({
                "summary": "Capture an implementation diff summary automatically",
                "rationale": "The final artifact review still needed a quick diff scan.",
                "suggested_follow_up": "Auto-generate a concise implementation diff summary for callers.",
            }),
            json!({
                "summary": "Capture an implementation diff summary automatically",
                "rationale": "The final artifact review still needed a quick diff scan.",
                "suggested_follow_up": "Auto-generate a concise implementation diff summary for callers.",
            }),
        ]),
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let handoff = runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    assert_eq!(handoff.improvement_opportunities.len(), 4);
    let planning_worker_improvement = handoff
        .improvement_opportunities
        .iter()
        .find(|summary| {
            summary.source.kind == "worker" && summary.source.stage.as_deref() == Some("planning")
        })
        .context("missing planning worker improvement summary")?;
    assert_eq!(
        planning_worker_improvement.source.role_id.as_deref(),
        Some("codex_planner")
    );
    assert_eq!(
        planning_worker_improvement.improvement_opportunities,
        vec![json!({
            "summary": "Document a planner-side checkpoint sizing heuristic",
            "rationale": "The planning worker had to reason from scratch about how small this checkpoint should be.",
            "suggested_follow_up": "Document one checkpoint sizing heuristic in the planner role prompt.",
        })]
    );
    let checkpoint_improvement = handoff
        .improvement_opportunities
        .iter()
        .find(|summary| summary.source.review_kind.as_deref() == Some("checkpoint"))
        .context("missing checkpoint improvement summary")?;
    assert_eq!(checkpoint_improvement.source.kind, "reviewer");
    assert_eq!(
        checkpoint_improvement.source.role_id.as_deref(),
        Some("codex_scope")
    );
    assert_eq!(
        checkpoint_improvement.improvement_opportunities,
        vec![json!({
            "summary": "Add a planning rubric example",
            "rationale": "The checkpoint contract is fine, but future planners would benefit from an explicit example.",
            "suggested_follow_up": "Add one canonical planning rubric example to the planner prompt.",
        })]
    );
    let artifact_improvement = handoff
        .improvement_opportunities
        .iter()
        .find(|summary| summary.source.review_kind.as_deref() == Some("artifact"))
        .context("missing artifact improvement summary")?;
    assert_eq!(artifact_improvement.source.kind, "reviewer");
    assert_eq!(
        artifact_improvement.source.role_id.as_deref(),
        Some("codex_checkpoint_contract")
    );
    assert_eq!(
        artifact_improvement.improvement_opportunities,
        vec![json!({
            "summary": "Capture an implementation diff summary automatically",
            "rationale": "The final artifact review still needed a quick diff scan.",
            "suggested_follow_up": "Auto-generate a concise implementation diff summary for callers.",
        })]
    );
    let artifact_worker_improvement = handoff
        .improvement_opportunities
        .iter()
        .find(|summary| {
            summary.source.kind == "worker" && summary.source.stage.as_deref() == Some("artifact")
        })
        .context("missing artifact worker improvement summary")?;
    assert_eq!(
        artifact_worker_improvement.source.role_id.as_deref(),
        Some("codex_implementer")
    );
    assert_eq!(
        artifact_worker_improvement.improvement_opportunities,
        vec![json!({
            "summary": "Emit a concise artifact verification report",
            "rationale": "The worker had the evidence, but the caller would benefit from a compact report artifact.",
            "suggested_follow_up": "Emit a concise artifact verification report alongside accepted candidates.",
        })]
    );

    let begin = runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    assert_eq!(
        serde_json::to_value(&begin.improvement_opportunities)?,
        serde_json::to_value(&handoff.improvement_opportunities)?,
    );

    git(workspace.path(), &["cherry-pick", &accepted_commit_sha])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let success = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id,
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some("replayed accepted artifact".to_owned()),
        },
    })?;
    assert_eq!(
        success["improvement_opportunities"],
        serde_json::to_value(&handoff.improvement_opportunities)?,
    );

    Ok(())
}

#[test]
fn repeated_handoff_and_begin_are_idempotent_before_terminal_success() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "idempotent caller finalize",
        "artifacts/idempotent.txt",
        "ok\n",
    )?;

    let first_handoff = runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    let second_handoff = runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    assert_eq!(first_handoff.phase, second_handoff.phase);
    assert_eq!(first_handoff.task_summary, second_handoff.task_summary);
    assert_eq!(
        first_handoff.artifact_summary.len(),
        second_handoff.artifact_summary.len()
    );
    assert_eq!(
        first_handoff.artifact_summary[0].accepted_commit_sha,
        second_handoff.artifact_summary[0].accepted_commit_sha
    );
    let handoff_event_count: i64 = Connection::open(workspace.path().join(".loopy/loopy.db"))?
        .query_row(
            "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__caller_finalize_handed_off'",
            params![accepted.loop_id.clone()],
            |row| row.get(0),
        )?;
    assert_eq!(handoff_event_count, 1);

    let first_begin = runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    let second_begin = runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    assert_eq!(first_begin.phase, second_begin.phase);
    assert_eq!(first_begin.task_summary, second_begin.task_summary);
    assert_eq!(
        first_begin.artifact_summary.len(),
        second_begin.artifact_summary.len()
    );
    assert_eq!(
        first_begin.artifact_summary[0].accepted_commit_sha,
        second_begin.artifact_summary[0].accepted_commit_sha
    );

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id,
    })?;
    assert_eq!(show.phase, "caller_finalizing");
    assert_eq!(
        show.caller_finalize
            .as_ref()
            .map(|summary| summary.status.as_str()),
        Some("active")
    );

    Ok(())
}

#[test]
fn handoff_is_rejected_after_caller_finalize_blocks_and_preserves_block_context() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "blocked caller finalize handoff guard",
        "artifacts/conflict.txt",
        "loop version\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.block_caller_finalize(BlockCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
        strategy_summary: "Attempted merge onto main".to_owned(),
        blocking_summary: "Conflict requires human choice".to_owned(),
        human_question: "Keep loop or caller version?".to_owned(),
        conflicting_files: vec!["README.md".to_owned()],
        notes: None,
        has_in_progress_integration: true,
    })?;

    let error = runtime
        .handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
            loop_id: accepted.loop_id.clone(),
        })
        .expect_err("expected repeated handoff to be rejected once caller finalize is blocked");
    assert!(
        error.to_string().contains("already started"),
        "unexpected error: {error:#}"
    );

    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id,
    })?;
    assert_eq!(show.phase, "caller_blocked_on_human");
    assert_eq!(
        show.caller_finalize
            .as_ref()
            .map(|summary| summary.status.as_str()),
        Some("blocked")
    );
    assert_eq!(
        show.caller_finalize
            .as_ref()
            .and_then(|summary| summary.human_question.as_deref()),
        Some("Keep loop or caller version?")
    );

    Ok(())
}

#[test]
fn finalize_success_accepts_merge_commit_landed_proof() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "merge strategy caller finalize",
        "artifacts/merged.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    git(
        workspace.path(),
        &[
            "merge",
            "--no-ff",
            "-m",
            "merge loop output",
            &accepted.branch,
        ],
    )?;
    let merge_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;

    let result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: accepted.loop_id,
        integration_summary: CallerIntegrationSummary {
            strategy: "merge".to_owned(),
            landed_commit_shas: vec![merge_head.clone()],
            resolution_notes: Some("Merged loop branch onto caller branch".to_owned()),
        },
    })?;

    assert_eq!(
        result["integration_summary"]["final_head_sha"],
        Value::String(merge_head)
    );

    Ok(())
}

#[test]
fn finalize_success_rejects_empty_landed_commit_proof() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "missing landed commit proof",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;

    let error = runtime
        .finalize_success(FinalizeSuccessRequest {
            loop_id: accepted.loop_id,
            integration_summary: CallerIntegrationSummary {
                strategy: "cherry_pick".to_owned(),
                landed_commit_shas: Vec::new(),
                resolution_notes: None,
            },
        })
        .expect_err("expected finalize-success to reject empty landed commit proof");
    assert!(
        error.to_string().contains("at least one landed commit"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn finalize_success_rejects_landed_commits_that_do_not_touch_deliverables() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "missing deliverable coverage",
        "artifacts/feature.txt",
        "implemented\n",
    )?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    let current_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;

    let error = runtime
        .finalize_success(FinalizeSuccessRequest {
            loop_id: accepted.loop_id,
            integration_summary: CallerIntegrationSummary {
                strategy: "cherry_pick".to_owned(),
                landed_commit_shas: vec![current_head],
                resolution_notes: None,
            },
        })
        .expect_err("expected finalize-success to reject landed commits that miss deliverables");
    assert!(
        error.to_string().contains("accepted deliverable paths"),
        "unexpected error: {error:#}"
    );

    Ok(())
}
