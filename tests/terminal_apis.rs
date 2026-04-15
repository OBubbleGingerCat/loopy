mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use loopy::{
    BeginCallerFinalizeRequest, CallerIntegrationSummary, CheckpointAcceptance,
    DeclareReviewBlockedRequest, DeclareWorkerBlockedRequest, FinalizeFailureRequest,
    FinalizeSuccessRequest, HandoffToCallerFinalizeRequest, OpenLoopRequest,
    OpenReviewRoundRequest, PrepareWorktreeRequest, ReviewKind, Runtime,
    StartReviewerInvocationRequest, StartWorkerInvocationRequest, SubmitArtifactReviewRequest,
    SubmitCandidateCommitRequest, SubmitCheckpointPlanRequest, SubmitCheckpointReviewRequest,
    WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::checkpoint;
use tempfile::TempDir;

#[test]
fn review_request_types_reject_legacy_json_shapes() -> Result<()> {
    let checkpoint_error = serde_json::from_value::<SubmitCheckpointReviewRequest>(json!({
        "invocation_context_path": "/tmp/checkpoint-review.json",
        "submission_id": "checkpoint-approve",
        "decision": "approve",
        "issues": ["Legacy advisory"],
        "notes": "legacy checkpoint review",
    }))
    .expect_err("legacy checkpoint review shape should be rejected");
    let checkpoint_error = checkpoint_error.to_string();
    assert!(
        checkpoint_error.contains("issues") || checkpoint_error.contains("summary"),
        "unexpected checkpoint review error: {checkpoint_error}"
    );

    let artifact_error = serde_json::from_value::<SubmitArtifactReviewRequest>(json!({
        "invocation_context_path": "/tmp/artifact-review.json",
        "submission_id": "artifact-reject",
        "decision": "reject",
        "issues": ["Legacy blocker"],
    }))
    .expect_err("legacy artifact review shape should be rejected");
    let artifact_error = artifact_error.to_string();
    assert!(
        artifact_error.contains("issues") || artifact_error.contains("summary"),
        "unexpected artifact review error: {artifact_error}"
    );

    Ok(())
}

#[test]
fn blocked_request_types_reject_legacy_json_shapes() -> Result<()> {
    let worker_error = serde_json::from_value::<DeclareWorkerBlockedRequest>(json!({
        "invocation_context_path": "/tmp/worker-blocked.json",
        "submission_id": "worker-blocked",
        "reason": "Missing build dependency",
        "blocking_type": "environment",
        "suggested_next_action": "Install the missing dependency and rerun the worker",
        "notes": "legacy worker blocked",
    }))
    .expect_err("legacy worker blocked shape should be rejected");
    let worker_error = worker_error.to_string();
    assert!(
        worker_error.contains("reason")
            || worker_error.contains("summary")
            || worker_error.contains("rationale"),
        "unexpected worker blocked error: {worker_error}"
    );

    let review_error = serde_json::from_value::<DeclareReviewBlockedRequest>(json!({
        "invocation_context_path": "/tmp/review-blocked.json",
        "submission_id": "review-blocked",
        "reason": "Static analysis service is down",
        "blocking_type": "dependency",
        "suggested_next_action": "Restore the service and rerun the review",
    }))
    .expect_err("legacy review blocked shape should be rejected");
    let review_error = review_error.to_string();
    assert!(
        review_error.contains("reason")
            || review_error.contains("summary")
            || review_error.contains("rationale"),
        "unexpected review blocked error: {review_error}"
    );

    Ok(())
}

#[test]
fn checkpoint_review_barrier_accepts_submitted_plan_and_promotes_checkpoint_state() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "review the plan",
        "checkpoint review approvals should promote the submitted plan",
        Some(vec!["codex_scope", "mock"]),
        None,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A"), checkpoint("Checkpoint B")],
        improvement_opportunities: None,
        notes: Some("first draft".to_owned()),
    })?;

    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let first_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;
    let second_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[1].clone(),
    })?;

    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &first_reviewer.invocation_id,
        ),
        submission_id: "review-1".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Looks good".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executable_before: Option<i64> = conn.query_row(
        "SELECT current_executable_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(executable_before, None);

    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &second_reviewer.invocation_id,
        ),
        submission_id: "review-2".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Ship it".to_owned(),
        notes: None,
    })?;

    let executable_after: Option<i64> = conn.query_row(
        "SELECT current_executable_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(executable_after, Some(1));

    let checkpoints: Vec<(i64, String, String)> = {
        let mut statement = conn.prepare(
            "SELECT sequence_index, checkpoint_id, title FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id.clone()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].0, 0);
    assert_eq!(checkpoints[0].2, "Checkpoint A");
    assert_eq!(checkpoints[1].0, 1);
    assert_eq!(checkpoints[1].2, "Checkpoint B");

    let accepted_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_accepted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(accepted_events, 1);

    Ok(())
}

#[test]
fn submit_checkpoint_plan_rejects_checkpoints_without_deliverables() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "invalid checkpoint schema",
        "checkpoint deliverables are required by schema",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let mut invalid_checkpoint = checkpoint("Checkpoint A");
    invalid_checkpoint.deliverables.clear();

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &worker.invocation_id,
            ),
            submission_id: "plan-submit".to_owned(),
            checkpoints: vec![invalid_checkpoint],
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected checkpoint plans without deliverables to be rejected");
    assert!(
        error.to_string().contains("deliverable"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn submit_checkpoint_plan_rejects_checkpoints_without_acceptance_steps() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "invalid checkpoint acceptance",
        "checkpoint acceptance metadata is required by schema",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let mut invalid_checkpoint = checkpoint("Checkpoint A");
    invalid_checkpoint.acceptance = CheckpointAcceptance {
        verification_steps: Vec::new(),
        expected_outcomes: invalid_checkpoint.acceptance.expected_outcomes.clone(),
    };

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &worker.invocation_id,
            ),
            submission_id: "plan-submit".to_owned(),
            checkpoints: vec![invalid_checkpoint],
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected checkpoints without acceptance verification steps to be rejected");
    assert!(
        error.to_string().contains("acceptance.verification_steps"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn review_blocked_fails_the_loop_immediately_and_rejects_later_reviewer_submissions() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "blocked review",
        "review blocked should fail the loop immediately",
        Some(vec!["codex_scope", "mock"]),
        None,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let blocked = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;
    let approving = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[1].clone(),
    })?;

    runtime.declare_review_blocked(DeclareReviewBlockedRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &blocked.invocation_id),
        submission_id: "blocked-1".to_owned(),
        summary: "Need a human product decision".to_owned(),
        rationale: "external_dependency".to_owned(),
        why_unrecoverable: "Escalate to coordinator".to_owned(),
        notes: Some("Waiting on product input".to_owned()),
    })?;
    let error = runtime
        .submit_checkpoint_review(SubmitCheckpointReviewRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &approving.invocation_id,
            ),
            submission_id: "approve-1".to_owned(),
            decision: "approve".to_owned(),
            blocking_issues: vec![],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "Technically fine".to_owned(),
            notes: None,
        })
        .expect_err(
            "expected later reviewer submission to be rejected once review blocked fails the loop",
        );
    assert!(
        error
            .to_string()
            .contains("cannot accept terminal submissions after caller finalize handoff")
            || error
                .to_string()
                .contains("cannot accept terminal submissions")
            || error.to_string().contains("loop status is failed"),
        "unexpected later reviewer submission error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (loop_status, round_status): (String, String) = conn.query_row(
        r#"
        SELECT loop.status, review.round_status
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__review_current review ON review.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1 AND review.review_round_id = ?2
        "#,
        params![loop_response.loop_id.clone(), review_round.review_round_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(loop_status, "failed");
    assert_eq!(round_status, "blocked");

    let failure_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__loop_failed'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(failure_events, 1);

    Ok(())
}

#[test]
fn review_blocked_closes_remaining_slots_and_rejects_new_reviewer_invocations() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "blocked review closes remaining slots",
        "a blocked review should close unopened reviewer slots",
        Some(vec!["codex_scope", "mock"]),
        None,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let blocked = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;

    runtime.declare_review_blocked(DeclareReviewBlockedRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &blocked.invocation_id),
        submission_id: "blocked-1".to_owned(),
        summary: "Need external clarification".to_owned(),
        rationale: "external_dependency".to_owned(),
        why_unrecoverable: "No reviewer can continue until the dependency is resolved".to_owned(),
        notes: None,
    })?;

    let error = runtime
        .start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: review_round.review_round_id.clone(),
            review_slot_id: review_round.review_slot_ids[1].clone(),
        })
        .expect_err("expected remaining reviewer slots to stop opening after blocked review");
    assert!(
        error.to_string().contains("loop status is failed")
            || error
                .to_string()
                .contains("cannot open reviewer invocations")
            || error.to_string().contains("review slot"),
        "unexpected reviewer open error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let slot_state_json: String = conn.query_row(
        "SELECT slot_state_json FROM SUBMIT_LOOP__review_current WHERE loop_id = ?1 AND review_round_id = ?2",
        params![loop_response.loop_id, review_round.review_round_id],
        |row| row.get(0),
    )?;
    let slot_state: Value = serde_json::from_str(&slot_state_json)?;
    let statuses = slot_state
        .as_array()
        .expect("slot_state_json should be an array")
        .iter()
        .map(|slot| slot["status"].as_str().unwrap_or("<missing>").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(statuses, vec!["blocked".to_owned(), "blocked".to_owned()]);

    Ok(())
}

#[test]
fn declare_worker_blocked_fails_the_loop() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "worker blocked",
        "worker blocked is terminal",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    runtime.declare_worker_blocked(DeclareWorkerBlockedRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "blocked-1".to_owned(),
        summary: "Missing build dependency".to_owned(),
        rationale: "environment".to_owned(),
        why_unrecoverable: "Restore the dependency".to_owned(),
        notes: Some("worker cannot proceed".to_owned()),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let loop_status: String = conn.query_row(
        "SELECT status FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(loop_status, "failed");

    let blocked_events: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 AND event_name IN ('SUBMIT_LOOP__worker_blocked_accepted', 'SUBMIT_LOOP__loop_failed') ORDER BY event_id ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(
        blocked_events,
        vec![
            "SUBMIT_LOOP__worker_blocked_accepted".to_owned(),
            "SUBMIT_LOOP__loop_failed".to_owned(),
        ]
    );

    Ok(())
}

#[test]
fn checkpoint_review_submission_persists_structured_issue_buckets() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "structured checkpoint review",
        "checkpoint review submissions should persist structured issue buckets",
        Some(vec!["codex_scope"]),
        None,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("first draft".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &reviewer.invocation_id),
        submission_id: "checkpoint-reject".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Define rollback criteria",
            "rationale": "Need explicit rollback criteria.",
            "expected_revision": "Add rollback criteria to the checkpoint plan.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Reject plan".to_owned(),
        notes: Some("Need explicit rollback criteria.".to_owned()),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let submission_content_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.submission_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__checkpoint_review_submitted'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get::<_, String>(0),
    )?;
    let submission_content: Value = serde_json::from_str(&submission_content_json)?;
    assert_eq!(submission_content["decision"], json!("reject"));
    assert_eq!(submission_content["summary"], json!("Reject plan"));
    assert_eq!(
        submission_content["blocking_issues"],
        json!([
            {
                "summary": "Define rollback criteria",
                "rationale": "Need explicit rollback criteria.",
                "expected_revision": "Add rollback criteria to the checkpoint plan."
            }
        ])
    );
    assert_eq!(submission_content["nonblocking_issues"], json!([]));
    assert_eq!(submission_content["improvement_opportunities"], json!([]));
    assert!(submission_content.get("issues").is_none());

    Ok(())
}

#[test]
fn submit_checkpoint_review_rejects_approve_with_blocking_issues() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "contradictory checkpoint review",
        "approve plus blocking issues must be rejected",
        Some(vec!["mock"]),
        None,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;

    let error = runtime
        .submit_checkpoint_review(SubmitCheckpointReviewRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &reviewer.invocation_id,
            ),
            submission_id: "approve-with-blockers".to_owned(),
            decision: "approve".to_owned(),
            blocking_issues: vec![json!({
                "summary": "Still blocked",
                "rationale": "A required dependency is missing.",
                "expected_revision": "Wait for the dependency and resubmit.",
            })],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "approve".to_owned(),
            notes: None,
        })
        .expect_err("expected approve checkpoint review with blocking issues to be rejected");
    assert!(
        error.to_string().contains("approve") && error.to_string().contains("blocking_issues"),
        "unexpected checkpoint review error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let review_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__checkpoint_review_submitted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(review_events, 0);

    Ok(())
}

#[test]
fn checkpoint_plan_submission_persists_worker_improvement_opportunities() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "worker plan improvements",
        "planning workers should persist caller-facing improvement opportunities",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: Some(vec![json!({
            "summary": "Add a planning example for repository-grounded checkpoints",
            "rationale": "This loop needed an extra pass to converge on the right checkpoint shape.",
            "suggested_follow_up": "Add a repository-grounded checkpoint example to the planner prompt.",
        })]),
        notes: Some("first draft".to_owned()),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let submission_content_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.submission_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__plan_submitted'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get::<_, String>(0),
    )?;
    let submission_content: Value = serde_json::from_str(&submission_content_json)?;
    assert_eq!(
        submission_content["improvement_opportunities"],
        json!([
            {
                "summary": "Add a planning example for repository-grounded checkpoints",
                "rationale": "This loop needed an extra pass to converge on the right checkpoint shape.",
                "suggested_follow_up": "Add a repository-grounded checkpoint example to the planner prompt."
            }
        ])
    );

    Ok(())
}

#[test]
fn blocked_submissions_persist_minimal_semantic_shape() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "structured blocked",
        "blocked submissions should persist summary rationale and why_unrecoverable only",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    runtime.declare_worker_blocked(DeclareWorkerBlockedRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "blocked-1".to_owned(),
        summary: "Missing build dependency".to_owned(),
        rationale: "Need the dependency restored before planning can continue.".to_owned(),
        why_unrecoverable: "No in-loop revision path remains.".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let blocked_content_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.submission_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__worker_blocked_accepted'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get::<_, String>(0),
    )?;
    let blocked_content: Value = serde_json::from_str(&blocked_content_json)?;
    assert_eq!(
        blocked_content["summary"],
        json!("Missing build dependency")
    );
    assert_eq!(
        blocked_content["rationale"],
        json!("Need the dependency restored before planning can continue.")
    );
    assert_eq!(
        blocked_content["why_unrecoverable"],
        json!("No in-loop revision path remains.")
    );
    assert!(blocked_content.get("reason").is_none());
    assert!(blocked_content.get("blocking_type").is_none());
    assert!(blocked_content.get("suggested_next_action").is_none());

    Ok(())
}

#[test]
fn worker_blocked_submissions_persist_optional_notes() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "blocked worker notes",
        "worker blocked submissions should persist notes when provided",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    runtime.declare_worker_blocked(DeclareWorkerBlockedRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "blocked-with-notes".to_owned(),
        summary: "Dependency mirror is unavailable".to_owned(),
        rationale: "The build cannot continue until the dependency mirror recovers.".to_owned(),
        why_unrecoverable: "No in-loop workaround can restore the mirror.".to_owned(),
        notes: Some("mirror outage started after the last successful retry".to_owned()),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let blocked_content_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.submission_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__worker_blocked_accepted'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get::<_, String>(0),
    )?;
    let blocked_content: Value = serde_json::from_str(&blocked_content_json)?;
    assert_eq!(
        blocked_content["notes"],
        json!("mirror outage started after the last successful retry")
    );

    Ok(())
}

#[test]
fn review_blocked_submissions_persist_optional_notes() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "blocked review notes",
        "review blocked submissions should persist notes when provided",
        Some(vec!["mock"]),
        Some(vec!["mock"]),
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("first draft".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;

    runtime.declare_review_blocked(DeclareReviewBlockedRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &reviewer.invocation_id),
        submission_id: "review-blocked-with-notes".to_owned(),
        summary: "Static analysis service is down".to_owned(),
        rationale: "The configured review dependency is unavailable.".to_owned(),
        why_unrecoverable: "This reviewer invocation cannot proceed without that service."
            .to_owned(),
        notes: Some("service returned 503 for the last three attempts".to_owned()),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let blocked_content_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.submission_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__review_blocked_recorded'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get::<_, String>(0),
    )?;
    let blocked_content: Value = serde_json::from_str(&blocked_content_json)?;
    assert_eq!(
        blocked_content["notes"],
        json!("service returned 503 for the last three attempts")
    );

    Ok(())
}

#[test]
fn submit_artifact_review_rejects_approve_with_blocking_issues() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "contradictory artifact review",
        "approve plus artifact blocking issues must be rejected",
        Some(vec!["mock"]),
        Some(vec!["mock"]),
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let planning = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &planning.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    let checkpoint_review = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let checkpoint_reviewer =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: checkpoint_review.review_round_id,
            review_slot_id: checkpoint_review.review_slot_ids[0].clone(),
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
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let deliverable = worktree_path.join("artifacts/checkpoint-a.txt");
    if let Some(parent) = deliverable.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&deliverable, "artifact output\n")?;
    git(&worktree_path, &["add", "artifacts/checkpoint-a.txt"])?;
    git(&worktree_path, &["commit", "-m", "implement checkpoint"])?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-submit".to_owned(),
        candidate_commit_sha: candidate_commit_sha,
        change_summary: json!({
            "headline": "Implemented checkpoint",
            "files": ["artifacts/checkpoint-a.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;
    let artifact_review = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id,
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review.review_round_id,
        review_slot_id: artifact_review.review_slot_ids[0].clone(),
    })?;

    let error = runtime
        .submit_artifact_review(SubmitArtifactReviewRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &artifact_reviewer.invocation_id,
            ),
            submission_id: "approve-with-blockers".to_owned(),
            decision: "approve".to_owned(),
            blocking_issues: vec![json!({
                "summary": "Build is not reproducible yet",
                "rationale": "The artifact still depends on a missing build input.",
                "expected_revision": "Include the missing build input and resubmit.",
            })],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "approved".to_owned(),
            notes: None,
        })
        .expect_err("expected approve artifact review with blocking issues to be rejected");
    assert!(
        error.to_string().contains("approve") && error.to_string().contains("blocking_issues"),
        "unexpected artifact review error: {error:#}"
    );

    let artifact_review_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__artifact_review_submitted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(artifact_review_events, 0);

    Ok(())
}

#[test]
fn artifact_approval_records_accepted_commit_and_builds_success_summary() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "artifact review",
        "artifact approval should produce a spec-correct success result",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract", "mock"]),
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Implement feature")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let worktree_path = workspace
        .path()
        .join(".loopy")
        .join("worktrees")
        .join(&loop_response.label);
    fs::write(worktree_path.join("feature.txt"), "implemented\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement feature checkpoint"],
    )?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: candidate_commit_sha.clone(),
        change_summary: json!({
            "headline": "Implemented feature checkpoint",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: Some("ready for artifact review".to_owned()),
    })?;

    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let artifact_reviewer_1 =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: artifact_review_round.review_round_id.clone(),
            review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
        })?;
    let artifact_reviewer_2 =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: artifact_review_round.review_round_id.clone(),
            review_slot_id: artifact_review_round.review_slot_ids[1].clone(),
        })?;
    for (submission_id, invocation_id) in [
        ("artifact-review-1", artifact_reviewer_1.invocation_id),
        ("artifact-review-2", artifact_reviewer_2.invocation_id),
    ] {
        runtime.submit_artifact_review(SubmitArtifactReviewRequest {
            invocation_context_path: invocation_context_path(workspace.path(), &invocation_id),
            submission_id: submission_id.to_owned(),
            decision: "approve".to_owned(),
            blocking_issues: vec![],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "approved".to_owned(),
            notes: None,
        })?;
    }

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    git(workspace.path(), &["cherry-pick", &candidate_commit_sha])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let success_result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head.clone()],
            resolution_notes: Some("Replay accepted artifact onto caller branch".to_owned()),
        },
    })?;

    assert_eq!(
        fs::read_to_string(workspace.path().join("feature.txt"))?,
        "implemented\n"
    );

    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );
    assert_eq!(
        success_result["artifact_summary"][0]["change_summary"],
        json!({
            "headline": "Implemented feature checkpoint",
            "files": ["feature.txt"],
        })
    );
    assert_eq!(
        success_result["artifact_summary"][0]["checkpoint_id"],
        Value::String(checkpoint_id.clone())
    );
    assert_eq!(
        success_result["commit_summary"][0]["commit_sha"],
        Value::String(candidate_commit_sha.clone())
    );
    assert_eq!(
        success_result["integration_summary"]["final_head_sha"],
        Value::String(landed_head)
    );
    assert_eq!(
        success_result["commit_summary"][0]["checkpoint_id"],
        Value::String(checkpoint_id.clone())
    );
    assert!(
        success_result["commit_summary"][0]["explanation"]
            .as_str()
            .is_some()
    );
    assert!(success_result.get("worktree_ref").is_none());

    let accepted_commit_sha: Option<String> = conn.query_row(
        "SELECT accepted_commit_sha FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 AND checkpoint_id = ?2",
        params![loop_response.loop_id, checkpoint_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        accepted_commit_sha.as_deref(),
        Some(candidate_commit_sha.as_str())
    );

    Ok(())
}

#[test]
fn candidate_commit_submission_persists_worker_improvement_opportunities() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "worker artifact improvements",
        "artifact workers should persist caller-facing improvement opportunities",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract"]),
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("artifact work".to_owned()),
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
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let worktree_path = workspace
        .path()
        .join(".loopy")
        .join("worktrees")
        .join(&loop_response.label);
    fs::write(worktree_path.join("feature.txt"), "implemented\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement feature checkpoint"],
    )?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha,
        change_summary: json!({
            "headline": "Implemented feature checkpoint",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: Some(vec![json!({
            "summary": "Capture a reusable artifact verification harness",
            "rationale": "The worker had to reconstruct the same local verification pattern manually.",
            "suggested_follow_up": "Add a reusable artifact verification harness for common checkpoint flows.",
        })]),
        notes: Some("ready for artifact review".to_owned()),
    })?;

    let submission_content_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.submission_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__candidate_commit_submitted'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get::<_, String>(0),
    )?;
    let submission_content: Value = serde_json::from_str(&submission_content_json)?;
    assert_eq!(
        submission_content["improvement_opportunities"],
        json!([
            {
                "summary": "Capture a reusable artifact verification harness",
                "rationale": "The worker had to reconstruct the same local verification pattern manually.",
                "suggested_follow_up": "Add a reusable artifact verification harness for common checkpoint flows."
            }
        ])
    );

    Ok(())
}

#[test]
fn checkpoint_review_rejection_records_plan_rejected_and_attempt_consumed() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "reject the plan",
        "checkpoint rejection should reject the plan revision",
        Some(vec!["codex_scope", "mock"]),
        None,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let reviewer_1 = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;
    let reviewer_2 = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[1].clone(),
    })?;

    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &reviewer_1.invocation_id,
        ),
        submission_id: "reject-1".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Needs another checkpoint",
            "rationale": "The plan still combines too much work into one checkpoint.",
            "expected_revision": "Split the plan into another checkpoint.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Rejected".to_owned(),
        notes: None,
    })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &reviewer_2.invocation_id,
        ),
        submission_id: "approve-1".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approve".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (review_status, loop_phase): (String, String) = conn.query_row(
        r#"
        SELECT review.round_status, loop.phase
        FROM SUBMIT_LOOP__review_current review
        JOIN SUBMIT_LOOP__loop_current loop ON loop.loop_id = review.loop_id
        WHERE review.loop_id = ?1
        ORDER BY review.updated_at DESC, review.review_round_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(review_status, "rejected");
    assert_eq!(loop_phase, "planning");

    let event_names: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 AND event_name IN ('SUBMIT_LOOP__plan_rejected', 'SUBMIT_LOOP__attempt_consumed') ORDER BY event_id ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(
        event_names,
        vec![
            "SUBMIT_LOOP__plan_rejected".to_owned(),
            "SUBMIT_LOOP__attempt_consumed".to_owned(),
        ]
    );

    Ok(())
}

#[test]
fn submit_checkpoint_plan_rejects_new_revisions_while_checkpoint_review_is_pending() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "stale planning review",
        "pending checkpoint review should freeze later plan submissions",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let first_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let second_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &first_worker.invocation_id,
        ),
        submission_id: "plan-1".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &second_worker.invocation_id,
            ),
            submission_id: "plan-2".to_owned(),
            checkpoints: vec![checkpoint("Checkpoint B")],
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected stale planning invocation to be rejected during checkpoint review");
    assert!(
        error.to_string().contains("checkpoint_review")
            || error.to_string().contains("pending review"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let latest_submitted_plan_revision: Option<i64> = conn.query_row(
        "SELECT latest_submitted_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(latest_submitted_plan_revision, Some(1));

    Ok(())
}

#[test]
fn submit_checkpoint_plan_rejects_submissions_after_loop_failure() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "stale failed plan",
        "failed loops should reject later checkpoint-plan submissions",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let blocking_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let stale_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    runtime.declare_worker_blocked(DeclareWorkerBlockedRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &blocking_worker.invocation_id,
        ),
        submission_id: "blocked-1".to_owned(),
        summary: "compiler missing".to_owned(),
        rationale: "environment".to_owned(),
        why_unrecoverable: "restore toolchain".to_owned(),
        notes: None,
    })?;

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &stale_worker.invocation_id,
            ),
            submission_id: "plan-after-failure".to_owned(),
            checkpoints: vec![checkpoint("Checkpoint A")],
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected failed loop to reject checkpoint-plan submissions");
    assert!(
        error.to_string().contains("failed") || error.to_string().contains("status"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let plan_submitted_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(plan_submitted_count, 0);

    Ok(())
}

#[test]
fn artifact_review_round_uses_the_snapshotted_candidate_commit() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "artifact snapshot",
        "artifact review must stay bound to the candidate it opened on",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract", "mock"]),
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Implement feature")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;

    let artifact_worker_1 = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    fs::write(worktree_path.join("feature.txt"), "v1\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate one"])?;
    let candidate_commit_sha_1 = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker_1.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: candidate_commit_sha_1.clone(),
        change_summary: json!({
            "headline": "Candidate one",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;

    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let artifact_reviewer_1 =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: artifact_review_round.review_round_id.clone(),
            review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
        })?;
    let artifact_reviewer_2 =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: artifact_review_round.review_round_id.clone(),
            review_slot_id: artifact_review_round.review_slot_ids[1].clone(),
        })?;

    let reviewer_context_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        params![artifact_reviewer_1.invocation_context_ref.clone()],
        |row| row.get(0),
    )?;
    let reviewer_context: Value = serde_json::from_str(&reviewer_context_json)?;
    assert_eq!(
        reviewer_context["review_target"]["candidate_commit_sha"],
        Value::String(candidate_commit_sha_1.clone())
    );

    for (submission_id, invocation_id) in [
        ("review-1", artifact_reviewer_1.invocation_id),
        ("review-2", artifact_reviewer_2.invocation_id),
    ] {
        runtime.submit_artifact_review(SubmitArtifactReviewRequest {
            invocation_context_path: invocation_context_path(workspace.path(), &invocation_id),
            submission_id: submission_id.to_owned(),
            decision: "approve".to_owned(),
            blocking_issues: vec![],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "approved".to_owned(),
            notes: None,
        })?;
    }

    let accepted_commit_sha: Option<String> = conn.query_row(
        "SELECT accepted_commit_sha FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 AND checkpoint_id = ?2",
        params![loop_response.loop_id.clone(), checkpoint_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(accepted_commit_sha, Some(candidate_commit_sha_1.clone()));

    let artifact_accepted_payload: String = conn.query_row(
        "SELECT payload_json FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__artifact_accepted' ORDER BY event_id DESC LIMIT 1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    let artifact_accepted: Value = serde_json::from_str(&artifact_accepted_payload)?;
    assert_eq!(
        artifact_accepted["candidate_commit_sha"],
        Value::String(candidate_commit_sha_1)
    );

    Ok(())
}

#[test]
fn submit_candidate_commit_rejects_new_candidates_once_artifact_review_is_pending() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "stale artifact review",
        "pending artifact review should freeze the candidate under review",
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;

    let first_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    fs::write(worktree_path.join("feature.txt"), "v1\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate one"])?;
    let candidate_commit_sha_1 = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &first_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: candidate_commit_sha_1,
        change_summary: json!({
            "headline": "Candidate one",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;
    runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;

    let second_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    fs::write(worktree_path.join("feature.txt"), "v2\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate two"])?;
    let candidate_commit_sha_2 = git_output(&worktree_path, &["rev-parse", "HEAD"])?;

    let error = runtime
        .submit_candidate_commit(SubmitCandidateCommitRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &second_worker.invocation_id,
            ),
            submission_id: "candidate-2".to_owned(),
            candidate_commit_sha: candidate_commit_sha_2,
            change_summary: json!({
                "headline": "Candidate two",
                "files": ["feature.txt"],
            }),
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected pending artifact review to reject a replacement candidate");
    assert!(
        error.to_string().contains("candidate_review")
            || error.to_string().contains("artifact review"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn submit_candidate_commit_rejects_stale_artifact_tokens_after_loop_failure() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "stale artifact token",
        "closed loops must reject terminal submissions from previously opened artifact workers",
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
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

    fs::write(worktree_path.join("feature.txt"), "closed-loop\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "closed loop candidate"])?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;

    runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: loop_response.loop_id.clone(),
        failure_cause_type: "coordinator_failure".to_owned(),
        summary: "the coordinator closed the loop".to_owned(),
    })?;

    let error = runtime
        .submit_candidate_commit(SubmitCandidateCommitRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &artifact_worker.invocation_id,
            ),
            submission_id: "candidate-after-failure".to_owned(),
            candidate_commit_sha,
            change_summary: json!({
                "headline": "Should be rejected",
            }),
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected closed loop to reject stale artifact token submissions");
    assert!(
        error.to_string().contains("status") || error.to_string().contains("closed"),
        "unexpected error: {error:#}"
    );

    let submitted_candidates: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__candidate_commit_submitted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(submitted_candidates, 0);
    let execution_state: String = conn.query_row(
        "SELECT execution_state FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 AND checkpoint_id = ?2",
        params![loop_response.loop_id, checkpoint_id],
        |row| row.get(0),
    )?;
    assert_eq!(execution_state, "pending");

    Ok(())
}

#[test]
fn artifact_review_rejection_revokes_candidate_commit_and_resets_checkpoint_state() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "artifact rejection",
        "artifact rejection should revoke the candidate and reset the checkpoint",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract", "mock"]),
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Implement feature")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
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
    fs::write(worktree_path.join("feature.txt"), "implemented\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate one"])?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: candidate_commit_sha.clone(),
        change_summary: json!({
            "headline": "Candidate one",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;

    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let artifact_reviewer_1 =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: artifact_review_round.review_round_id.clone(),
            review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
        })?;
    let artifact_reviewer_2 =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            review_round_id: artifact_review_round.review_round_id.clone(),
            review_slot_id: artifact_review_round.review_slot_ids[1].clone(),
        })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer_1.invocation_id,
        ),
        submission_id: "reject-1".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Needs changes",
            "rationale": "The candidate still fails the artifact review contract.",
            "expected_revision": "Revise the candidate commit to address the review findings.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "reject".to_owned(),
        notes: None,
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer_2.invocation_id,
        ),
        submission_id: "approve-1".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approve".to_owned(),
        notes: None,
    })?;

    let (execution_state, candidate_commit, accepted_commit): (String, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT execution_state, candidate_commit_sha, accepted_commit_sha FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 AND checkpoint_id = ?2",
            params![loop_response.loop_id.clone(), checkpoint_id.clone()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    assert_eq!(execution_state, "pending");
    assert_eq!(candidate_commit, None);
    assert_eq!(accepted_commit, None);

    let commit_lifecycle: String = conn.query_row(
        "SELECT lifecycle FROM SUBMIT_LOOP__commit_current WHERE loop_id = ?1 AND checkpoint_id = ?2 AND commit_sha = ?3",
        params![loop_response.loop_id.clone(), checkpoint_id, candidate_commit_sha],
        |row| row.get(0),
    )?;
    assert_eq!(commit_lifecycle, "revoked");

    let event_names: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 AND event_name IN ('SUBMIT_LOOP__artifact_rejected', 'SUBMIT_LOOP__candidate_commit_revoked', 'SUBMIT_LOOP__attempt_consumed') ORDER BY event_id ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(
        event_names,
        vec![
            "SUBMIT_LOOP__artifact_rejected".to_owned(),
            "SUBMIT_LOOP__candidate_commit_revoked".to_owned(),
            "SUBMIT_LOOP__attempt_consumed".to_owned(),
        ]
    );

    Ok(())
}

#[test]
fn submit_candidate_commit_requires_previous_checkpoint_to_be_accepted() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "checkpoint lineage",
        "later checkpoints must stay blocked until the previous checkpoint is accepted",
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Checkpoint A"), checkpoint("Checkpoint B")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_ids: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id.clone()], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(checkpoint_ids.len(), 2);

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_ids[1].clone()),
    })?;
    fs::write(worktree_path.join("feature.txt"), "later checkpoint work\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "candidate for checkpoint two"],
    )?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;

    let error = runtime
        .submit_candidate_commit(SubmitCandidateCommitRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &artifact_worker.invocation_id,
            ),
            submission_id: "candidate-1".to_owned(),
            candidate_commit_sha,
            change_summary: json!({
                "headline": "Checkpoint B implementation",
                "files": ["feature.txt"],
            }),
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected second checkpoint candidate to stay blocked");
    assert!(
        error.to_string().contains("previous checkpoint") || error.to_string().contains("accepted"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn submit_candidate_commit_rejects_reopening_an_earlier_checkpoint_after_a_later_one_is_accepted()
-> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "checkpoint reorder",
        "accepted later checkpoints should freeze earlier checkpoint lineage",
    ))?;
    let worktree_path = prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

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
        checkpoints: vec![checkpoint("Checkpoint A"), checkpoint("Checkpoint B")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let checkpoint_ids: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id.clone()], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(checkpoint_ids.len(), 2);

    for (submission_id, checkpoint_id, file_name, commit_message) in [
        (
            "candidate-1",
            checkpoint_ids[0].clone(),
            "checkpoint-a.txt",
            "checkpoint a",
        ),
        (
            "candidate-2",
            checkpoint_ids[1].clone(),
            "checkpoint-b.txt",
            "checkpoint b",
        ),
    ] {
        let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Artifact,
            checkpoint_id: Some(checkpoint_id.clone()),
        })?;
        fs::write(worktree_path.join(file_name), format!("{commit_message}\n"))?;
        git(&worktree_path, &["add", file_name])?;
        git(&worktree_path, &["commit", "-m", commit_message])?;
        let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
        runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &artifact_worker.invocation_id,
            ),
            submission_id: submission_id.to_owned(),
            candidate_commit_sha,
            change_summary: json!({
                "headline": commit_message,
                "files": [file_name],
            }),
            improvement_opportunities: None,
            notes: None,
        })?;

        let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
            loop_id: loop_response.loop_id.clone(),
            review_kind: ReviewKind::Artifact,
            target_type: "checkpoint_id".to_owned(),
            target_ref: checkpoint_id,
        })?;
        let artifact_reviewer =
            runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
                loop_id: loop_response.loop_id.clone(),
                review_round_id: artifact_review_round.review_round_id,
                review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
            })?;
        runtime.submit_artifact_review(SubmitArtifactReviewRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &artifact_reviewer.invocation_id,
            ),
            submission_id: format!("review-{submission_id}"),
            decision: "approve".to_owned(),
            blocking_issues: vec![],
            nonblocking_issues: None,
            improvement_opportunities: None,
            summary: "approved".to_owned(),
            notes: None,
        })?;
    }

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_ids[0].clone()),
    })?;
    fs::write(worktree_path.join("checkpoint-a.txt"), "checkpoint a v2\n")?;
    git(&worktree_path, &["add", "checkpoint-a.txt"])?;
    git(&worktree_path, &["commit", "-m", "checkpoint a v2"])?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;

    let error = runtime
        .submit_candidate_commit(SubmitCandidateCommitRequest {
            invocation_context_path: invocation_context_path(
                workspace.path(),
                &reopened_worker.invocation_id,
            ),
            submission_id: "candidate-reopen".to_owned(),
            candidate_commit_sha,
            change_summary: json!({
                "headline": "checkpoint a v2",
                "files": ["checkpoint-a.txt"],
            }),
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected reopening checkpoint A to be blocked after checkpoint B accepted");
    assert!(
        error.to_string().contains("later checkpoint")
            || error.to_string().contains("accepted after"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn submit_candidate_commit_accepts_commits_from_mirrored_gitdir_fallback_worktree() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "mirror gitdir candidate",
        "candidate validation should use the fallback worktree gitdir",
    ))?;
    let worktree_path = materialize_loop_worktree_with_mirrored_gitdir(
        workspace.path(),
        &loop_response.branch,
        &loop_response.label,
    )?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

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
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
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
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
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
    fs::write(
        worktree_path.join("feature.txt"),
        "implemented via mirrored gitdir\n",
    )?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "candidate from mirrored gitdir"],
    )?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;

    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: candidate_commit_sha.clone(),
        change_summary: json!({
            "headline": "Candidate from mirrored gitdir",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;

    let stored_candidate_commit_sha: Option<String> = conn.query_row(
        "SELECT candidate_commit_sha FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 AND checkpoint_id = ?2",
        params![loop_response.loop_id, checkpoint_id],
        |row| row.get(0),
    )?;
    assert_eq!(stored_candidate_commit_sha, Some(candidate_commit_sha));

    Ok(())
}

fn invocation_context_path(workspace_root: &Path, invocation_id: &str) -> PathBuf {
    workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"))
}

fn open_loop_request(summary: &str, context: &str) -> OpenLoopRequest {
    open_loop_request_with_reviewers(
        summary,
        context,
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract"]),
    )
}

fn open_loop_request_with_reviewers(
    summary: &str,
    context: &str,
    checkpoint_reviewers: Option<Vec<&str>>,
    artifact_reviewers: Option<Vec<&str>>,
) -> OpenLoopRequest {
    OpenLoopRequest {
        summary: summary.to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(context.to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: checkpoint_reviewers
            .map(|ids| ids.into_iter().map(str::to_owned).collect()),
        artifact_reviewers: artifact_reviewers
            .map(|ids| ids.into_iter().map(str::to_owned).collect()),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    }
}

fn materialize_loop_worktree_with_mirrored_gitdir(
    workspace_root: &Path,
    branch: &str,
    label: &str,
) -> Result<PathBuf> {
    let worktree_path = workspace_root.join(".loopy").join("worktrees").join(label);
    let mirror_path = workspace_root
        .join(".loopy")
        .join(format!("git-common-{label}"));
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&mirror_path)?;

    let copy_output = Command::new("cp")
        .args(["-a", ".git/.", mirror_path.to_str().unwrap()])
        .current_dir(workspace_root)
        .output()
        .context("failed to copy primary gitdir into mirrored fallback")?;
    if !copy_output.status.success() {
        bail!(
            "cp -a .git/. {} failed\nstdout:\n{}\nstderr:\n{}",
            mirror_path.display(),
            String::from_utf8_lossy(&copy_output.stdout),
            String::from_utf8_lossy(&copy_output.stderr)
        );
    }

    let git_dir_arg = format!("--git-dir={}", mirror_path.display());
    let work_tree_arg = format!("--work-tree={}", workspace_root.display());
    let output = Command::new("git")
        .args([
            git_dir_arg.as_str(),
            work_tree_arg.as_str(),
            "worktree",
            "add",
            "-b",
            branch,
            worktree_path.to_str().unwrap(),
            "HEAD",
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to materialize mirrored-gitdir worktree")?;
    if !output.status.success() {
        bail!(
            "git worktree add with mirrored gitdir failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(worktree_path)
}

fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn install_bundle_into_workspace(workspace_root: &Path) -> Result<PathBuf> {
    let install_root = workspace_root
        .join(".loopy")
        .join("installed-skills")
        .join("loopy-submit-loop");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    install_fake_codex_command(workspace_root, &install_root)?;
    Ok(install_root)
}

fn prepare_loop_worktree(runtime: &Runtime, loop_id: &str) -> Result<PathBuf> {
    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_id.to_owned(),
    })?;
    let path = prepared["path"]
        .as_str()
        .context("prepare-worktree response missing path")?;
    Ok(PathBuf::from(path))
}

fn install_fake_codex_command(workspace_root: &Path, install_root: &Path) -> Result<()> {
    let fake_bin_dir = workspace_root.join(".loopy").join("fake-bin");
    fs::create_dir_all(&fake_bin_dir)?;
    let fake_codex = fake_bin_dir.join("codex");
    fs::write(
        &fake_codex,
        "#!/bin/bash\nwhile IFS= read -r _; do :; done\nprintf '{}\\n'\n",
    )?;
    let chmod_output = Command::new("chmod")
        .args([
            "755",
            fake_codex.to_str().context("non-utf8 fake codex path")?,
        ])
        .output()
        .context("failed to chmod fake codex command")?;
    if !chmod_output.status.success() {
        bail!(
            "chmod fake codex failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&chmod_output.stdout),
            String::from_utf8_lossy(&chmod_output.stderr)
        );
    }

    let manifest_path = install_root.join("submit-loop.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let fake_codex_str = fake_codex
        .to_str()
        .context("non-utf8 fake codex path")?
        .to_owned();
    let updated = manifest.replace(
        "command = \"codex\"",
        &format!("command = \"{fake_codex_str}\""),
    );
    if updated == manifest {
        bail!(
            "failed to rewrite codex executor commands in {}",
            manifest_path.display()
        );
    }
    fs::write(&manifest_path, updated)?;
    Ok(())
}

fn git_workspace() -> Result<TempDir> {
    let workspace = tempfile::tempdir()?;
    Command::new("git")
        .arg("init")
        .arg("--initial-branch=main")
        .current_dir(workspace.path())
        .output()
        .context("failed to initialize git repository")?;
    Command::new("git")
        .args(["config", "user.name", "Codex"])
        .current_dir(workspace.path())
        .output()
        .context("failed to configure git user.name")?;
    Command::new("git")
        .args(["config", "user.email", "codex@example.com"])
        .current_dir(workspace.path())
        .output()
        .context("failed to configure git user.email")?;
    fs::write(workspace.path().join("README.md"), "seed\n")?;
    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(workspace.path())
        .output()
        .context("failed to stage README.md")?;
    Command::new("git")
        .args(["commit", "-m", "seed"])
        .current_dir(workspace.path())
        .output()
        .context("failed to create seed commit")?;
    Ok(workspace)
}
