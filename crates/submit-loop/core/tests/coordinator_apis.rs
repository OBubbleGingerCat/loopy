mod support;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use anyhow::{Context, Result};
use loopy::{
    BeginCallerFinalizeRequest, CallerIntegrationSummary, FinalizeSuccessRequest,
    HandoffToCallerFinalizeRequest, OpenLoopRequest, OpenReviewRoundRequest,
    PrepareWorktreeRequest, ReviewKind, Runtime, StartReviewerInvocationRequest,
    StartWorkerInvocationRequest, SubmitArtifactReviewRequest, SubmitCandidateCommitRequest,
    SubmitCheckpointPlanRequest, SubmitCheckpointReviewRequest, WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::{checkpoint, materialize_loop_worktree_with_mirrored_gitdir};
use tempfile::TempDir;

#[test]
fn finalize_success_rejects_loops_without_accepted_commit_state() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = Runtime::with_installed_skill_root(
        workspace.path(),
        crate::support::submit_loop_source_root().as_path(),
    )?;
    let loop_response = runtime.open_loop(open_loop_request(
        "worktree lifecycle",
        "success should require accepted artifact and commit state",
    ))?;

    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let current_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let error = runtime
        .finalize_success(FinalizeSuccessRequest {
            loop_id: loop_response.loop_id.clone(),
            integration_summary: CallerIntegrationSummary {
                strategy: "cherry_pick".to_owned(),
                landed_commit_shas: vec![current_head],
                resolution_notes: None,
            },
        })
        .expect_err(
            "expected success result to reject loops outside caller-finalizing success flow",
        );
    assert!(
        error
            .to_string()
            .contains("cannot finalize success from loop phase"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (loop_status, loop_phase): (String, String) = conn.query_row(
        "SELECT status, phase FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(loop_status, "open");
    assert_eq!(loop_phase, "planning");

    let worktree_event_names: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 AND event_name LIKE 'SUBMIT_LOOP__worktree_%' ORDER BY event_id ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()?
    };
    assert_eq!(
        worktree_event_names,
        vec!["SUBMIT_LOOP__worktree_prepared".to_owned()]
    );
    let result_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__result_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(result_rows, 0);

    Ok(())
}

#[test]
fn open_review_round_allocates_fixed_pending_slots() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "review round".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("coordinator should allocate reviewer slots up front".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["codex_scope".to_owned(), "mock".to_owned()]),
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    submit_basic_checkpoint_plan(workspace.path(), &runtime, &loop_response.loop_id)?;

    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;

    assert_eq!(review_round.review_slot_ids.len(), 2);
    assert!(
        review_round
            .review_slot_ids
            .iter()
            .all(|slot_id| slot_id.starts_with("slot-"))
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (review_kind, round_status, target_type, target_ref, slot_state_json): (
        String,
        String,
        String,
        String,
        String,
    ) = conn.query_row(
        "SELECT review_kind, round_status, target_type, target_ref, slot_state_json FROM SUBMIT_LOOP__review_current WHERE loop_id = ?1 AND review_round_id = ?2",
        params![loop_response.loop_id, review_round.review_round_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    assert_eq!(review_kind, "checkpoint");
    assert_eq!(round_status, "pending");
    assert_eq!(target_type, "plan_revision");
    assert_eq!(target_ref, "plan-1");
    let slot_state: Value = serde_json::from_str(&slot_state_json)?;
    let slots = slot_state
        .as_array()
        .context("slot_state_json must be an array")?;
    assert_eq!(slots.len(), 2);
    assert!(
        slots
            .iter()
            .all(|slot| slot["status"] == Value::String("pending".to_owned()))
    );

    Ok(())
}

#[test]
fn open_review_round_rejects_duplicate_pending_rounds_for_the_same_target() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "duplicate review round".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("a second pending round for the same target should be rejected".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["codex_scope".to_owned(), "mock".to_owned()]),
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    submit_basic_checkpoint_plan(workspace.path(), &runtime, &loop_response.loop_id)?;

    let first_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    assert_eq!(first_round.review_slot_ids.len(), 2);

    let error = runtime
        .open_review_round(OpenReviewRoundRequest {
            loop_id: loop_response.loop_id.clone(),
            review_kind: ReviewKind::Checkpoint,
            target_type: "plan_revision".to_owned(),
            target_ref: "plan-1".to_owned(),
        })
        .expect_err("expected duplicate pending review round to be rejected");
    assert!(
        error.to_string().contains("pending review round")
            || error.to_string().contains("already exists"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let review_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__review_current WHERE loop_id = ?1 AND review_kind = 'checkpoint' AND target_type = 'plan_revision' AND target_ref = 'plan-1'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(review_rows, 1);

    Ok(())
}

#[test]
fn open_review_round_rejects_already_executable_plan_revisions() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "stale accepted plan review",
        "an already executable plan revision must not reopen checkpoint review",
    ))?;
    submit_basic_checkpoint_plan(workspace.path(), &runtime, &loop_response.loop_id)?;

    let first_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: first_round.review_round_id,
        review_slot_id: first_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &reviewer.invocation_id),
        submission_id: "review-1".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let error = runtime
        .open_review_round(OpenReviewRoundRequest {
            loop_id: loop_response.loop_id.clone(),
            review_kind: ReviewKind::Checkpoint,
            target_type: "plan_revision".to_owned(),
            target_ref: "plan-1".to_owned(),
        })
        .expect_err(
            "expected already executable plan revision to reject a new checkpoint review round",
        );
    assert!(
        error.to_string().contains("executable")
            || error.to_string().contains("accepted")
            || error.to_string().contains("stale"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let review_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__review_current WHERE loop_id = ?1 AND review_kind = 'checkpoint' AND target_type = 'plan_revision' AND target_ref = 'plan-1'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(review_rows, 1);

    Ok(())
}

#[test]
fn open_review_round_does_not_persist_projection_rows_when_commit_fails() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "review round",
        "failed review round open must not leak state",
    ))?;
    submit_basic_checkpoint_plan(workspace.path(), &runtime, &loop_response.loop_id)?;
    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_review_round_open
        BEFORE INSERT ON CORE__events
        WHEN NEW.event_name = 'SUBMIT_LOOP__review_round_opened'
        BEGIN
            SELECT RAISE(ABORT, 'forced review-round-open failure');
        END;
        "#,
    )?;

    let error = runtime
        .open_review_round(OpenReviewRoundRequest {
            loop_id: loop_response.loop_id.clone(),
            review_kind: ReviewKind::Checkpoint,
            target_type: "plan_revision".to_owned(),
            target_ref: "plan-1".to_owned(),
        })
        .expect_err("expected review round open to fail");
    assert!(
        error
            .to_string()
            .contains("forced review-round-open failure"),
        "unexpected error: {error:#}"
    );

    let event_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__review_round_opened'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let review_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__review_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(event_rows, 0);
    assert_eq!(review_rows, 0);

    Ok(())
}

#[test]
fn open_artifact_review_round_rejects_checkpoints_without_a_candidate_commit() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "artifact review",
        "artifact review must bind to an existing candidate commit",
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

    let error = runtime
        .open_review_round(OpenReviewRoundRequest {
            loop_id: loop_response.loop_id,
            review_kind: ReviewKind::Artifact,
            target_type: "checkpoint_id".to_owned(),
            target_ref: checkpoint_id,
        })
        .expect_err("expected artifact review round opening to require a candidate commit");
    assert!(
        error.to_string().contains("candidate commit"),
        "unexpected error: {error:#}"
    );

    let installed_worker = fs::read_to_string(
        install_root.join("roles/coding-task/planning_worker/codex_planner.md"),
    )?;
    assert!(installed_worker.contains("Coding Task Planning Worker"));

    Ok(())
}

#[test]
fn finalize_success_requires_every_active_checkpoint_to_be_accepted() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "multi-checkpoint success gate",
        "success must wait for every active checkpoint in the executable plan",
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
    let checkpoints: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id.clone()], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoints[0].clone()),
    })?;
    fs::write(worktree_path.join("feature-a.txt"), "implemented\n")?;
    git(&worktree_path, &["add", "feature-a.txt"])?;
    git(&worktree_path, &["commit", "-m", "Implement checkpoint A"])?;
    let candidate_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: candidate_commit_sha.clone(),
        change_summary: json!({
            "headline": "Implemented checkpoint A",
            "files": ["feature-a.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;
    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoints[0].clone(),
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let current_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let error = runtime
        .finalize_success(FinalizeSuccessRequest {
            loop_id: loop_response.loop_id.clone(),
            integration_summary: CallerIntegrationSummary {
                strategy: "cherry_pick".to_owned(),
                landed_commit_shas: vec![current_head],
                resolution_notes: None,
            },
        })
        .expect_err("expected success result to require caller-finalizing phase");
    assert!(
        error
            .to_string()
            .contains("cannot finalize success from loop phase"),
        "unexpected error: {error:#}"
    );

    let loop_succeeded_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__loop_succeeded'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let result_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__result_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(loop_succeeded_events, 0);
    assert_eq!(result_rows, 0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn finalize_success_allows_cleanup_warning_after_integration() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "cleanup warning success",
        "post-integration worktree cleanup warnings should not fail the loop",
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
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
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
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let worktrees_dir = workspace.path().join(".loopy").join("worktrees");
    let original_permissions = fs::metadata(&worktrees_dir)?.permissions();
    let mut readonly_permissions = original_permissions.clone();
    readonly_permissions.set_mode(0o555);
    fs::set_permissions(&worktrees_dir, readonly_permissions)?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    git(workspace.path(), &["cherry-pick", &candidate_commit_sha])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let finalize_result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some("Caller-owned finalize for cleanup warning coverage".to_owned()),
        },
    });
    fs::set_permissions(&worktrees_dir, original_permissions)?;
    let success_result = finalize_result?;

    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );
    assert_eq!(
        success_result["cleanup_warnings"][0]["summary"],
        Value::String(format!(
            "failed to remove disposable worktree {} after cleanup retries",
            worktree_path.display()
        ))
    );
    assert_eq!(
        success_result["cleanup_warnings"][0]["worktree_ref"]["path"],
        Value::String(worktree_path.display().to_string())
    );
    assert_eq!(
        success_result["commit_summary"][0]["commit_sha"],
        Value::String(candidate_commit_sha)
    );

    let (loop_status, loop_phase, worktree_lifecycle): (String, String, String) = conn.query_row(
        r#"
        SELECT loop.status, loop.phase, worktree.lifecycle
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__worktree_current worktree ON worktree.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1
        "#,
        params![loop_response.loop_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(loop_status, "succeeded");
    assert_eq!(loop_phase, "completed");
    assert_eq!(worktree_lifecycle, "cleanup_warning");

    Ok(())
}

#[test]
fn finalize_success_removes_mirrored_gitdir_fallback_state_after_out_of_band_worktree_removal()
-> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "mirrored cleanup success",
        "finalize-success should clean mirrored fallback worktrees without warnings",
    ))?;
    let worktree_path = materialize_loop_worktree_with_mirrored_gitdir(
        workspace.path(),
        &loop_response.branch,
        &loop_response.label,
    )?;
    let mirror_path = workspace
        .path()
        .join(".loopy")
        .join(format!("git-common-{}", loop_response.label));
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
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
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
        "implemented via mirror\n",
    )?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement mirrored cleanup checkpoint"],
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
            "headline": "Mirrored cleanup candidate",
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
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    git(
        workspace.path(),
        &[
            "fetch",
            mirror_path.to_str().unwrap(),
            &candidate_commit_sha,
        ],
    )?;
    git(workspace.path(), &["cherry-pick", "FETCH_HEAD"])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    fs::remove_dir_all(&worktree_path)?;
    assert!(!worktree_path.try_exists()?);
    assert!(mirror_path.try_exists()?);
    let success_result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some(
                "Caller-owned finalize after mirrored worktree removal".to_owned(),
            ),
        },
    })?;

    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );
    assert!(success_result.get("cleanup_warnings").is_none());
    assert_eq!(
        success_result["commit_summary"][0]["commit_sha"],
        Value::String(candidate_commit_sha)
    );
    assert!(!worktree_path.try_exists()?);
    assert!(!mirror_path.try_exists()?);

    let (loop_status, loop_phase, worktree_lifecycle): (String, String, String) = conn.query_row(
        r#"
        SELECT loop.status, loop.phase, worktree.lifecycle
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__worktree_current worktree ON worktree.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1
        "#,
        params![loop_response.loop_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(loop_status, "succeeded");
    assert_eq!(loop_phase, "completed");
    assert_eq!(worktree_lifecycle, "deleted");

    Ok(())
}

#[test]
fn finalize_success_removes_primary_worktree_registration_after_out_of_band_worktree_removal()
-> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "primary cleanup success",
        "finalize-success should deregister prunable primary worktrees without warnings",
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
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
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
        "implemented via primary gitdir\n",
    )?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement primary cleanup checkpoint"],
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
            "headline": "Primary cleanup candidate",
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
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    git(workspace.path(), &["cherry-pick", &candidate_commit_sha])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;

    fs::remove_dir_all(&worktree_path)?;
    assert!(!worktree_path.try_exists()?);
    let registration_before = git_output(workspace.path(), &["worktree", "list", "--porcelain"])?;
    let worktree_line = format!("worktree {}", worktree_path.display());
    assert!(
        registration_before.contains(&worktree_line),
        "expected git to keep the removed worktree registered before finalize-success:\n{registration_before}"
    );

    let success_result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some(
                "Caller-owned finalize after primary worktree removal".to_owned(),
            ),
        },
    })?;

    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );
    assert!(success_result.get("cleanup_warnings").is_none());
    assert_eq!(
        success_result["commit_summary"][0]["commit_sha"],
        Value::String(candidate_commit_sha)
    );

    let registration_after = git_output(workspace.path(), &["worktree", "list", "--porcelain"])?;
    assert!(
        !registration_after.contains(&worktree_line),
        "expected finalize-success to remove the prunable worktree registration:\n{registration_after}"
    );

    let (loop_status, loop_phase, worktree_lifecycle): (String, String, String) = conn.query_row(
        r#"
        SELECT loop.status, loop.phase, worktree.lifecycle
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__worktree_current worktree ON worktree.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1
        "#,
        params![loop_response.loop_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(loop_status, "succeeded");
    assert_eq!(loop_phase, "completed");
    assert_eq!(worktree_lifecycle, "deleted");

    Ok(())
}

#[test]
fn finalize_success_ignores_stale_mirrored_gitdir_when_cleaning_primary_registration() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "primary cleanup with stale mirror",
        "finalize-success should fall back to the primary gitdir when git-common is stale",
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
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
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
        "implemented with stale mirror present\n",
    )?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement stale mirror cleanup checkpoint"],
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
            "headline": "Primary cleanup candidate with stale mirror",
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
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    git(workspace.path(), &["cherry-pick", &candidate_commit_sha])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;

    fs::remove_dir_all(&worktree_path)?;
    assert!(!worktree_path.try_exists()?);
    fs::create_dir_all(
        workspace
            .path()
            .join(".loopy")
            .join(format!("git-common-{}", loop_response.label)),
    )?;

    let success_result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some(
                "Caller-owned finalize with stale mirrored gitdir fallback".to_owned(),
            ),
        },
    })?;

    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );
    assert!(success_result.get("cleanup_warnings").is_none());
    let registration_after = git_output(workspace.path(), &["worktree", "list", "--porcelain"])?;
    assert!(
        !registration_after.contains(&format!("worktree {}", worktree_path.display())),
        "expected finalize-success to remove the primary registration even with a stale mirror dir:\n{registration_after}"
    );

    Ok(())
}

#[test]
fn prepare_worktree_rejects_already_succeeded_loops() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "prepare after success",
        "prepare-worktree should reject loops that already succeeded",
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
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
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
        "implemented before success\n",
    )?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement feature before success"],
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
            "headline": "Implemented before success",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: Some("ready for artifact review".to_owned()),
    })?;

    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id,
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let success_result = caller_finalize_success(
        &runtime,
        workspace.path(),
        &loop_response.loop_id,
        &candidate_commit_sha,
        "Caller-owned finalize for terminal success coverage",
    )?;
    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );

    let error = runtime
        .prepare_worktree(PrepareWorktreeRequest {
            loop_id: loop_response.loop_id,
        })
        .expect_err("expected prepare-worktree to reject an already-succeeded loop");
    assert!(
        error
            .to_string()
            .contains("cannot prepare worktree when loop status is succeeded"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn finalize_success_removes_orphaned_worktree_directories_when_registration_is_missing()
-> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "orphaned worktree cleanup",
        "finalize-success should delete orphaned worktree directories when git registration is gone",
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
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
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
        "implemented before orphan cleanup\n",
    )?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(
        &worktree_path,
        &["commit", "-m", "Implement orphan cleanup checkpoint"],
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
            "headline": "Orphan cleanup candidate",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: Some("ready for artifact review".to_owned()),
    })?;

    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id,
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    git(workspace.path(), &["cherry-pick", &candidate_commit_sha])?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;

    git(
        workspace.path(),
        &[
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap(),
        ],
    )?;
    fs::create_dir_all(&worktree_path)?;
    fs::write(
        worktree_path.join("orphan.txt"),
        "left behind after registration loss\n",
    )?;
    assert!(worktree_path.try_exists()?);

    let success_result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_response.loop_id.clone(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some(
                "Caller-owned finalize after orphaned worktree registration cleanup".to_owned(),
            ),
        },
    })?;

    assert_eq!(
        success_result["status"],
        Value::String("success".to_owned())
    );
    assert!(success_result.get("cleanup_warnings").is_none());
    assert!(
        !worktree_path.try_exists()?,
        "expected finalize-success to remove the orphaned worktree directory"
    );

    Ok(())
}

#[test]
fn prepare_worktree_preserves_artifact_phase_when_repairing_missing_worktrees() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "preserve artifact phase",
        "prepare-worktree should not rewind artifact-phase loops back to planning",
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
    let phase_before: String = conn.query_row(
        "SELECT phase FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(phase_before, "artifact");

    git(
        workspace.path(),
        &[
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap(),
        ],
    )?;
    assert!(!worktree_path.try_exists()?);

    let repaired = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    assert_eq!(repaired["lifecycle"].as_str(), Some("prepared"));

    let phase_after: String = conn.query_row(
        "SELECT phase FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(phase_after, "artifact");

    let planning_error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("planning workers should remain blocked after artifact-phase repair");
    assert!(
        planning_error
            .to_string()
            .contains("current phase is artifact"),
        "unexpected planning worker error: {planning_error:#}"
    );

    Ok(())
}

#[test]
fn prepare_worktree_rejects_unregistered_checkouts_that_only_match_by_branch_name() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "reject branch-only adoption",
        "prepare-worktree should not adopt an unregistered checkout just because HEAD matches the loop branch",
    ))?;
    let worktree_path = workspace
        .path()
        .join(".loopy")
        .join("worktrees")
        .join(&loop_response.label);
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let clone_output = Command::new("git")
        .args([
            "clone",
            workspace.path().to_str().unwrap(),
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(workspace.path())
        .output()?;
    assert!(
        clone_output.status.success(),
        "git clone failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&clone_output.stdout),
        String::from_utf8_lossy(&clone_output.stderr)
    );
    git(&worktree_path, &["checkout", "-b", &loop_response.branch])?;

    let prepare_result = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id,
    })?;

    assert_eq!(prepare_result["status"].as_str(), Some("failure"));
    assert_eq!(
        prepare_result["failure_cause_type"].as_str(),
        Some("worktree_prepare_failed")
    );

    Ok(())
}

fn invocation_context_path(
    workspace_root: &std::path::Path,
    invocation_id: &str,
) -> std::path::PathBuf {
    workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"))
}

fn open_loop_request(summary: &str, context: &str) -> OpenLoopRequest {
    OpenLoopRequest {
        summary: summary.to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(context.to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["codex_scope".to_owned()]),
        artifact_reviewers: Some(vec!["codex_checkpoint_contract".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    }
}

fn submit_basic_checkpoint_plan(
    workspace_root: &std::path::Path,
    runtime: &Runtime,
    loop_id: &str,
) -> Result<()> {
    prepare_loop_worktree(runtime, loop_id)?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_id.to_owned(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace_root, &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: None,
    })?;
    Ok(())
}

fn install_bundle_into_workspace(workspace_root: &std::path::Path) -> Result<std::path::PathBuf> {
    let install_root = workspace_root
        .join(".loopy")
        .join("installed-skills")
        .join("loopy-submit-loop");
    let repo_root = crate::support::repo_root().as_path();
    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        anyhow::bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    install_fake_codex_command(workspace_root, &install_root)?;
    Ok(install_root)
}

fn prepare_loop_worktree(runtime: &Runtime, loop_id: &str) -> Result<std::path::PathBuf> {
    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_id.to_owned(),
    })?;
    let path = prepared["path"]
        .as_str()
        .context("prepare-worktree response missing path")?;
    Ok(std::path::PathBuf::from(path))
}

fn install_fake_codex_command(
    workspace_root: &std::path::Path,
    install_root: &std::path::Path,
) -> Result<()> {
    let fake_bin_dir = workspace_root.join(".loopy").join("fake-bin");
    fs::create_dir_all(&fake_bin_dir)?;
    let fake_codex = fake_bin_dir.join("codex");
    fs::write(
        &fake_codex,
        "#!/bin/bash\nwhile IFS= read -r _; do :; done\nprintf '{}\\n'\n",
    )?;
    let mut perms = fs::metadata(&fake_codex)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fake_codex, perms)?;

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
        anyhow::bail!(
            "failed to rewrite codex executor commands in {}",
            manifest_path.display()
        );
    }
    fs::write(&manifest_path, updated)?;
    Ok(())
}

fn caller_finalize_success(
    runtime: &Runtime,
    workspace_root: &std::path::Path,
    loop_id: &str,
    accepted_commit_sha: &str,
    resolution_notes: &str,
) -> Result<Value> {
    runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
        loop_id: loop_id.to_owned(),
    })?;
    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: loop_id.to_owned(),
    })?;
    git(workspace_root, &["cherry-pick", accepted_commit_sha])?;
    let landed_head = git_output(workspace_root, &["rev-parse", "HEAD"])?;
    runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: loop_id.to_owned(),
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some(resolution_notes.to_owned()),
        },
    })
}

fn git(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn git_output(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
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
