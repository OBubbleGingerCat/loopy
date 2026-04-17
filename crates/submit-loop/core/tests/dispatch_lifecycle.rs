mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use loopy::{
    OpenLoopRequest, OpenReviewRoundRequest, PrepareWorktreeRequest, ReviewKind, Runtime,
    ShowLoopRequest, StartReviewerInvocationRequest, StartWorkerInvocationRequest,
    SubmitArtifactReviewRequest, SubmitCandidateCommitRequest, SubmitCheckpointPlanRequest,
    SubmitCheckpointReviewRequest, WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::{checkpoint, ensure_prompt_covers_required_help_flags};
use tempfile::TempDir;

#[test]
fn start_worker_invocation_materializes_requests_records_lifecycle_events_and_transcripts()
-> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch worker",
        "exercise local-command dispatch",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(invocation.accepted_terminal_api, None);
    assert!(invocation.transcript_segment_count > 0);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let lifecycle_events: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 ORDER BY loop_seq ASC",
        )?;
        let rows = statement.query_map([&loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()?
    };
    assert!(
        lifecycle_events.contains(&"CORE__request_materialized".to_owned()),
        "missing CORE__request_materialized"
    );
    assert!(
        lifecycle_events.contains(&"CORE__dispatch_started".to_owned()),
        "missing CORE__dispatch_started"
    );
    assert!(
        lifecycle_events.contains(&"CORE__response_received".to_owned()),
        "missing CORE__response_received"
    );
    assert!(
        lifecycle_events.contains(&"CORE__invocation_failed".to_owned()),
        "expected invocation failure when mock executor exits without a terminal API call"
    );

    let transcript_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__transcript_segments WHERE invocation_id = ?1",
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    assert!(
        transcript_count > 0,
        "expected transcript segments to be stored"
    );
    let stdin_payload = load_mock_executor_stdin_payload(&conn, &invocation.invocation_id)?;
    assert!(stdin_payload.contains("\"role_prompt_markdown\""));
    assert!(stdin_payload.contains("\"runtime_prompt_markdown\""));
    assert!(stdin_payload.contains("\"invocation_prompt_markdown\""));
    assert!(stdin_payload.contains("Do not use any skill"));
    assert!(stdin_payload.contains("Do not ask the user"));
    assert!(stdin_payload.contains(
        "planning worker invocation must finish by calling exactly one allowed terminal API"
    ));
    assert!(
        stdin_payload
            .contains("A checkpoint is a durable deliverable boundary or state transition")
    );
    assert!(stdin_payload.contains(
        "Each checkpoint must describe one artifact boundary using `title`, `kind`, `deliverables`, and `acceptance`"
    ));
    assert!(stdin_payload.contains(
        "Put verification commands in `acceptance.verification_steps` and behavioral goals in `acceptance.expected_outcomes`"
    ));
    assert!(stdin_payload.contains(
        "Treat routine verification as part of the checkpoint that produces the artifact"
    ));
    assert!(stdin_payload.contains(
        "Do not create a standalone checkpoint whose only purpose is syntax checking, test execution, local validation, or confirming expected behavior"
    ));
    assert!(
        stdin_payload.contains("deliverables[].type")
            && (stdin_payload.contains("type = file")
                || stdin_payload.contains("`type` currently must be `file`")
                || stdin_payload.contains("\"type\": \"file\"")),
        "planning worker prompt should document that deliverables include a `type` field and currently require file deliverables"
    );
    assert!(
        has_nearby_schema_fields(
            &stdin_payload,
            "\"suggested_follow_up\"",
            &["\"summary\"", "\"rationale\""],
            200,
        ),
        "planning worker prompt should document the improvement_opportunities entry shape"
    );
    assert!(stdin_payload.contains(
        "submit-checkpoint-plan --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --checkpoints-json <json_array> --improvement-opportunities-json <json_array>"
    ));
    assert!(stdin_payload.contains(
        "declare-worker-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>"
    ));
    assert!(stdin_payload.contains(
        "request-timeout-extension --invocation-context-path invocation_context.invocation_context_path --requested-timeout-sec <seconds> --progress-summary <summary> --rationale <rationale>"
    ));
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "submit-checkpoint-plan",
    )?;
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "declare-worker-blocked",
    )?;
    assert!(stdin_payload.contains(&invocation.invocation_id));
    assert!(
        !stdin_payload.contains("submit-checkpoint-review"),
        "planning worker prompt should not mention reviewer terminal APIs"
    );

    Ok(())
}

#[test]
fn start_reviewer_invocation_includes_runtime_prompt_contract_for_reviewer_invocations()
-> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "dispatch reviewer".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("exercise reviewer runtime prompt contract".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: workspace
            .path()
            .join(".loopy")
            .join("invocations")
            .join(format!("{}.json", planning_worker.invocation_id)),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("reviewable plan".to_owned()),
    })?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
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

    assert_eq!(reviewer.accepted_terminal_api, None);
    assert!(reviewer.transcript_segment_count > 0);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdin_payload = load_mock_executor_stdin_payload(&conn, &reviewer.invocation_id)?;
    assert!(stdin_payload.contains("\"role_prompt_markdown\""));
    assert!(stdin_payload.contains("\"runtime_prompt_markdown\""));
    assert!(stdin_payload.contains("\"invocation_prompt_markdown\""));
    assert!(stdin_payload.contains("Do not use any skill"));
    assert!(stdin_payload.contains("Do not ask the user"));
    assert!(
        stdin_payload.contains(
            "reviewer invocation must finish by calling exactly one allowed terminal API"
        )
    );
    assert!(
        stdin_payload
            .contains("Reject plans that split routine verification into standalone checkpoints")
    );
    assert!(stdin_payload.contains(
        "Prefer plans where verification is attached to the artifact-producing checkpoint"
    ));
    assert!(
        stdin_payload
            .contains("Reject plans whose checkpoints omit deliverables or acceptance metadata")
    );
    assert!(
        has_nearby_schema_fields(
            &stdin_payload,
            "\"expected_revision\"",
            &["\"summary\"", "\"rationale\""],
            200,
        ),
        "checkpoint reviewer prompt should document the full issue entry shape"
    );
    assert!(
        has_nearby_schema_fields(
            &stdin_payload,
            "\"suggested_follow_up\"",
            &["\"summary\"", "\"rationale\""],
            200,
        ),
        "checkpoint reviewer prompt should document the full improvement entry shape"
    );
    assert!(
        stdin_payload.contains("approve reviews cannot include blocking_issues")
            || stdin_payload.contains("`approve` must not include any `blocking_issues`"),
        "checkpoint reviewer prompt should document the approve versus blocking_issues rule"
    );
    assert!(stdin_payload.contains(
        "submit-checkpoint-review --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --decision <approve|reject> --summary <summary> --blocking-issues-json <json_array> --nonblocking-issues-json <json_array> --improvement-opportunities-json <json_array>"
    ));
    assert!(stdin_payload.contains(
        "declare-review-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>"
    ));
    assert!(stdin_payload.contains(
        "request-timeout-extension --invocation-context-path invocation_context.invocation_context_path --requested-timeout-sec <seconds> --progress-summary <summary> --rationale <rationale>"
    ));
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "submit-checkpoint-review",
    )?;
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "declare-review-blocked",
    )?;
    assert!(stdin_payload.contains("\"review_kind\": \"checkpoint\""));
    assert!(
        !stdin_payload.contains("submit-candidate-commit"),
        "checkpoint reviewer prompt should not mention artifact worker terminal APIs"
    );

    Ok(())
}

#[test]
fn start_artifact_worker_invocation_includes_exact_terminal_api_cli_forms() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "dispatch artifact worker".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("exercise artifact worker runtime prompt contract".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    let (_, checkpoint_id) =
        advance_loop_to_artifact_phase(&runtime, workspace.path(), &loop_response.loop_id)?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id),
    })?;

    assert_eq!(artifact_worker.accepted_terminal_api, None);
    assert!(artifact_worker.transcript_segment_count > 0);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdin_payload = load_mock_executor_stdin_payload(&conn, &artifact_worker.invocation_id)?;
    assert!(
        stdin_payload.contains("`bound_checkpoint`")
            && stdin_payload.contains("\"checkpoint_id\"")
            && stdin_payload.contains("\"sequence_index\"")
            && stdin_payload.contains("\"title\"")
            && stdin_payload.contains("\"kind\"")
            && stdin_payload.contains("\"deliverables\"")
            && stdin_payload.contains("\"acceptance\""),
        "artifact worker prompt should document the authoritative bound_checkpoint shape"
    );
    assert!(stdin_payload.contains(
        "submit-candidate-commit --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --candidate-commit-sha <candidate_commit_sha> --change-summary-json <json_object> --improvement-opportunities-json <json_array>"
    ));
    assert!(
        stdin_payload.contains("change_summary_json must be a JSON object")
            && stdin_payload.contains("\"headline\"")
            && stdin_payload.contains("\"files\""),
        "artifact worker prompt should document the canonical change_summary_json object shape"
    );
    assert!(stdin_payload.contains(
        "declare-worker-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>"
    ));
    assert!(stdin_payload.contains(
        "request-timeout-extension --invocation-context-path invocation_context.invocation_context_path --requested-timeout-sec <seconds> --progress-summary <summary> --rationale <rationale>"
    ));
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "submit-candidate-commit",
    )?;
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "declare-worker-blocked",
    )?;
    assert!(stdin_payload.contains("\"stage\": \"artifact\""));

    Ok(())
}

#[test]
fn reopened_planning_worker_prompt_requires_revision_against_review_history() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "reopen planning prompt".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("reopened planning workers should revise against review history".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
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
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &reviewer.invocation_id),
        submission_id: "checkpoint-reject".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Define rollback criteria",
            "rationale": "The plan needs explicit rollback criteria.",
            "expected_revision": "Add rollback criteria to the checkpoint plan.",
        })],
        nonblocking_issues: Some(vec![json!({
            "summary": "Prefer one canonical validation command",
            "rationale": "A single canonical command reduces reviewer ambiguity.",
            "expected_revision": "Document one canonical validation command for the checkpoint.",
        })]),
        improvement_opportunities: None,
        summary: "plan needs revision".to_owned(),
        notes: Some("reopen planning worker".to_owned()),
    })?;

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdin_payload = load_mock_executor_stdin_payload(&conn, &reopened_worker.invocation_id)?;
    assert!(stdin_payload.contains("`invocation_context.review_history.latest_result`"));
    assert!(stdin_payload.contains(
        "If `invocation_context.review_history.latest_result` is not null, revise against that review result before resubmitting"
    ));
    assert!(stdin_payload.contains(
        "Address reviewer `blocking_issues` before resubmitting and incorporate `nonblocking_issues` when they materially improve the bound work without expanding scope"
    ));
    assert!(
        stdin_payload.contains("\"review_round_id\"")
            && stdin_payload.contains("\"review_kind\"")
            && stdin_payload.contains("\"round_status\"")
            && stdin_payload.contains("\"target_type\"")
            && stdin_payload.contains("\"target_ref\"")
            && stdin_payload.contains("\"target_metadata\"")
            && stdin_payload.contains("\"summary\"")
            && stdin_payload.contains("\"blocking_issues\"")
            && stdin_payload.contains("\"nonblocking_issues\""),
        "planning worker prompt should document the review_history.latest_result shape used for revision guidance"
    );

    Ok(())
}

#[test]
fn reopened_artifact_worker_prompt_requires_revision_against_review_history() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "reopen artifact prompt".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("reopened artifact workers should revise against review history".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    let (worktree_path, checkpoint_id) =
        advance_loop_to_artifact_phase(&runtime, workspace.path(), &loop_response.loop_id)?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let candidate_commit_sha = create_candidate_commit(
        &worktree_path,
        "artifact.txt",
        "candidate payload\n",
        "artifact candidate",
    )?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "artifact-submit".to_owned(),
        candidate_commit_sha,
        change_summary: json!({
            "headline": "artifact candidate",
            "files": ["artifact.txt"],
        }),
        improvement_opportunities: None,
        notes: Some("first candidate".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &reviewer.invocation_id),
        submission_id: "artifact-reject".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Preserve failure exit semantics",
            "rationale": "The candidate changes failure semantics in JSON mode.",
            "expected_revision": "Restore the original failure exit-code behavior.",
        })],
        nonblocking_issues: Some(vec![json!({
            "summary": "Tighten regression coverage",
            "rationale": "A focused regression test would make the fix easier to audit.",
            "expected_revision": "Add a regression test for JSON-mode failure exits.",
        })]),
        improvement_opportunities: None,
        summary: "artifact needs revision".to_owned(),
        notes: Some("reopen artifact worker".to_owned()),
    })?;

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdin_payload = load_mock_executor_stdin_payload(&conn, &reopened_worker.invocation_id)?;
    assert!(stdin_payload.contains("`invocation_context.review_history.latest_result`"));
    assert!(stdin_payload.contains(
        "If `invocation_context.review_history.latest_result` is not null, revise against that review result before resubmitting"
    ));
    assert!(stdin_payload.contains(
        "Address reviewer `blocking_issues` before resubmitting and incorporate `nonblocking_issues` when they materially improve the bound work without expanding scope"
    ));

    Ok(())
}

#[test]
fn start_artifact_reviewer_invocation_includes_exact_terminal_api_cli_forms() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "dispatch artifact reviewer".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("exercise artifact reviewer runtime prompt contract".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    let (worktree_path, checkpoint_id) =
        advance_loop_to_artifact_phase(&runtime, workspace.path(), &loop_response.loop_id)?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let candidate_commit_sha = create_candidate_commit(
        &worktree_path,
        "artifact.txt",
        "artifact payload\n",
        "artifact candidate",
    )?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "artifact-submit".to_owned(),
        candidate_commit_sha,
        change_summary: json!({
            "headline": "artifact candidate",
            "files": ["artifact.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;

    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id,
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;

    assert_eq!(artifact_reviewer.accepted_terminal_api, None);
    assert!(artifact_reviewer.transcript_segment_count > 0);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdin_payload = load_mock_executor_stdin_payload(&conn, &artifact_reviewer.invocation_id)?;
    assert!(
        stdin_payload.contains("`review_target`")
            && stdin_payload.contains("\"type\"")
            && stdin_payload.contains("\"ref\"")
            && stdin_payload.contains("\"checkpoint_id\"")
            && stdin_payload.contains("\"sequence_index\"")
            && stdin_payload.contains("\"title\"")
            && stdin_payload.contains("\"kind\"")
            && stdin_payload.contains("\"deliverables\"")
            && stdin_payload.contains("\"acceptance\"")
            && stdin_payload.contains("\"candidate_commit_sha\""),
        "artifact reviewer prompt should document the artifact review_target shape"
    );
    assert!(
        has_nearby_schema_fields(
            &stdin_payload,
            "\"expected_revision\"",
            &["\"summary\"", "\"rationale\""],
            200,
        ),
        "artifact reviewer prompt should document the full issue entry shape"
    );
    assert!(
        has_nearby_schema_fields(
            &stdin_payload,
            "\"suggested_follow_up\"",
            &["\"summary\"", "\"rationale\""],
            200,
        ),
        "artifact reviewer prompt should document the full improvement entry shape"
    );
    assert!(
        stdin_payload.contains("approve reviews cannot include blocking_issues")
            || stdin_payload.contains("`approve` must not include any `blocking_issues`"),
        "artifact reviewer prompt should document the approve versus blocking_issues rule"
    );
    assert!(stdin_payload.contains(
        "submit-artifact-review --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --decision <approve|reject> --summary <summary> --blocking-issues-json <json_array> --nonblocking-issues-json <json_array> --improvement-opportunities-json <json_array>"
    ));
    assert!(stdin_payload.contains(
        "declare-review-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>"
    ));
    assert!(stdin_payload.contains(
        "request-timeout-extension --invocation-context-path invocation_context.invocation_context_path --requested-timeout-sec <seconds> --progress-summary <summary> --rationale <rationale>"
    ));
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "submit-artifact-review",
    )?;
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &stdin_payload,
        "declare-review-blocked",
    )?;
    assert!(stdin_payload.contains("\"review_kind\": \"artifact\""));
    assert!(
        !stdin_payload.contains("submit-candidate-commit"),
        "artifact reviewer prompt should not mention artifact worker terminal APIs"
    );

    Ok(())
}

#[test]
fn show_loop_updated_at_advances_when_plan_submission_changes_read_state() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "show-loop freshness",
        "plan submissions should advance the summary freshness token",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let before = runtime.show_loop(ShowLoopRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: workspace
            .path()
            .join(".loopy")
            .join("invocations")
            .join(format!("{}.json", planning_worker.invocation_id)),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("freshness coverage".to_owned()),
    })?;

    let after = runtime.show_loop(ShowLoopRequest {
        loop_id: loop_response.loop_id,
    })?;

    assert_eq!(
        after
            .plan
            .as_ref()
            .and_then(|plan| plan.latest_submitted_plan_revision),
        Some(1)
    );
    assert_eq!(
        after
            .plan
            .as_ref()
            .and_then(|plan| plan.current_executable_plan_revision),
        None
    );
    assert_ne!(
        before.updated_at, after.updated_at,
        "show-loop freshness token should advance when plan state changes"
    );

    Ok(())
}

#[test]
fn start_worker_invocation_launch_failure_records_terminal_invocation_failure() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        Path::new("/definitely/missing/mock-worker"),
        &[],
        60,
        None,
    )?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch launch failure",
        "prepared worktree plus missing executor should surface a launch failure",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected dispatch launch to fail when the executor command is missing");
    assert!(
        error
            .to_string()
            .contains("/definitely/missing/mock-worker")
            || error.to_string().contains("failed to run"),
        "unexpected launch failure: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let lifecycle_events: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 ORDER BY loop_seq ASC",
        )?;
        let rows = statement.query_map([&loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()?
    };
    assert!(
        lifecycle_events.contains(&"CORE__invocation_failed".to_owned()),
        "expected durable invocation failure event on launch failure"
    );

    let invocation_id: String = conn.query_row(
        "SELECT invocation_id FROM CORE__invocation_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    let status: String = conn.query_row(
        "SELECT status FROM CORE__invocation_current WHERE invocation_id = ?1",
        params![invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(status, "failed");

    Ok(())
}

#[test]
fn start_worker_invocation_accepts_mock_checkpoint_plan_and_consumes_token() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_submit_plan_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch accepted plan",
        "mock worker should submit a checkpoint plan",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(
        invocation.accepted_terminal_api.as_deref(),
        Some("SUBMIT_LOOP__submit_checkpoint_plan")
    );
    assert!(invocation.transcript_segment_count > 0);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let accepted_api: Option<String> = conn.query_row(
        "SELECT accepted_api FROM CORE__invocation_current WHERE invocation_id = ?1",
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        accepted_api.as_deref(),
        Some("SUBMIT_LOOP__submit_checkpoint_plan")
    );
    let token_state: String = conn.query_row(
        "SELECT token_state FROM CORE__capability_current WHERE invocation_id = ?1",
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(token_state, "consumed");
    let latest_plan_revision: Option<i64> = conn.query_row(
        "SELECT latest_submitted_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(latest_plan_revision, Some(1));

    Ok(())
}

#[test]
fn start_worker_invocation_returns_terminal_result_for_blocked_workers() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &install_root.join("bin/loopy-submit-loop"),
        &[
            "declare-worker-blocked".to_owned(),
            "--invocation-context-path".to_owned(),
            "{invocation_context_path}".to_owned(),
            "--submission-id".to_owned(),
            "mock-blocked-worker".to_owned(),
            "--summary".to_owned(),
            "Missing build dependency".to_owned(),
            "--rationale".to_owned(),
            "environment".to_owned(),
            "--why-unrecoverable".to_owned(),
            "Restore the dependency".to_owned(),
        ],
        60,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch blocked worker",
        "blocked workers should return the terminal failure payload immediately",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(
        invocation.accepted_terminal_api.as_deref(),
        Some("SUBMIT_LOOP__declare_worker_blocked")
    );
    let terminal_result = invocation
        .terminal_result
        .context("expected blocked worker to return terminal result")?;
    assert_eq!(
        terminal_result["status"],
        Value::String("failure".to_owned())
    );
    assert_eq!(
        terminal_result["failure_cause_type"],
        Value::String("worker_blocked".to_owned())
    );
    assert_eq!(
        terminal_result["loop_id"],
        Value::String(loop_response.loop_id)
    );

    Ok(())
}

#[test]
fn submit_checkpoint_plan_reuses_same_submission_id_without_duplicate_effects() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "idempotent plan submit",
        "same submission id should not duplicate effects",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let invocation_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", invocation.invocation_id));

    let first = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path.clone(),
        submission_id: "same-submission".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("first submit".to_owned()),
    })?;
    let second = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path,
        submission_id: "same-submission".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("retry submit".to_owned()),
    })?;

    assert!(!first.idempotent);
    assert!(second.idempotent);
    assert_eq!(first.plan_revision, second.plan_revision);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let terminal_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__terminal_api_called'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(terminal_event_count, 1);
    let plan_submitted_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(plan_submitted_count, 1);

    Ok(())
}

#[test]
fn submit_checkpoint_plan_rejects_different_submission_id_after_token_consumption() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "consumed token rejects new submission",
        "a consumed checkpoint-plan token must not accept a different submission id",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let invocation_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", invocation.invocation_id));

    let first = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path.clone(),
        submission_id: "submission-1".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("first submit".to_owned()),
    })?;
    assert!(!first.idempotent);

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path,
            submission_id: "submission-2".to_owned(),
            checkpoints: vec![checkpoint("Checkpoint A")],
            improvement_opportunities: None,
            notes: Some("should be rejected".to_owned()),
        })
        .expect_err("expected a consumed token to reject a different submission id");
    assert!(
        error.to_string().contains("already consumed"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let terminal_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__terminal_api_called'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(terminal_event_count, 1);
    let plan_submitted_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(plan_submitted_count, 1);

    Ok(())
}

#[test]
fn submit_checkpoint_plan_rejects_tampered_invocation_context() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let source_loop = runtime.open_loop(open_loop_request(
        "source loop",
        "tampered invocation context should be rejected",
    ))?;
    let target_loop = runtime.open_loop(open_loop_request(
        "target loop",
        "should not receive forged events",
    ))?;
    prepare_loop_worktree(&runtime, &source_loop.loop_id)?;
    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: source_loop.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let invocation_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", invocation.invocation_id));
    let mut persisted_context: Value =
        serde_json::from_str(&fs::read_to_string(&invocation_context_path)?)?;
    persisted_context["loop_id"] = Value::String(target_loop.loop_id.clone());
    fs::write(
        &invocation_context_path,
        serde_json::to_string_pretty(&persisted_context)?,
    )?;

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path,
            submission_id: "forged-submission".to_owned(),
            checkpoints: vec![checkpoint("Checkpoint A")],
            improvement_opportunities: None,
            notes: Some("forged context".to_owned()),
        })
        .expect_err("expected tampered invocation context to be rejected");
    assert!(
        error.to_string().contains("invocation context"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let target_terminal_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__terminal_api_called'",
        params![target_loop.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(target_terminal_count, 0);
    let target_plan_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'",
        params![target_loop.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(target_plan_count, 0);
    let token_state: String = conn.query_row(
        "SELECT token_state FROM CORE__capability_current WHERE invocation_id = ?1",
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(token_state, "available");

    Ok(())
}

#[test]
fn submit_checkpoint_plan_replays_original_revision_after_later_submission() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "idempotent revision replay",
        "retries must return the original accepted revision",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let first_invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let second_invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let first_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", first_invocation.invocation_id));
    let second_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", second_invocation.invocation_id));

    let first = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: first_context_path.clone(),
        submission_id: "submission-1".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("first submit".to_owned()),
    })?;
    let second = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: second_context_path,
        submission_id: "submission-2".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint B")],
        improvement_opportunities: None,
        notes: Some("second submit".to_owned()),
    })?;
    let replay = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: first_context_path,
        submission_id: "submission-1".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("retry submit".to_owned()),
    })?;

    assert_eq!(first.plan_revision, 1);
    assert_eq!(second.plan_revision, 2);
    assert!(replay.idempotent);
    assert_eq!(replay.plan_revision, first.plan_revision);

    Ok(())
}

#[test]
fn start_worker_invocation_inherits_parent_environment_when_bypass_sandbox_is_enabled() -> Result<()>
{
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("dump-home-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
python3 -c 'import json, os, sys; print(json.dumps({"home": os.environ.get("HOME"), "stdin_payload": sys.stdin.read()}))'
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(&install_root, &script_path, &[], 60, None)?;

    let runtime = Runtime::new(workspace.path())?;
    let mut request = open_loop_request(
        "dispatch bypass env inheritance",
        "bypassed child process should inherit the parent environment",
    );
    request.bypass_sandbox = Some(true);
    let loop_response = runtime.open_loop(request)?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let expected_home = std::env::var("HOME").context("HOME must be set for this test")?;
    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(invocation.accepted_terminal_api, None);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdout_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stdout'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    let stdout_payload: Value = serde_json::from_str(&stdout_payload_json)?;
    let executor_stdout: Value = serde_json::from_str(
        stdout_payload["text"]
            .as_str()
            .context("missing executor stdout text")?,
    )?;
    assert_eq!(executor_stdout["home"], Value::String(expected_home));
    assert!(
        executor_stdout["stdin_payload"]
            .as_str()
            .context("missing stdin payload echo")?
            .contains("\"role_prompt_markdown\"")
    );

    let executor_config = read_content(&conn, &invocation.executor_config_ref)?;
    assert_eq!(executor_config["env_policy"], json!("inherit_all"));

    Ok(())
}

#[test]
fn start_worker_invocation_rejects_invalid_persisted_env_policy_string() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = write_stdin_echo_worker_script(workspace.path(), "echo-worker.sh")?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(&install_root, &script_path, &[], 60, None)?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch invalid persisted env policy string",
        "dispatch should reject unsupported persisted env policies",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    install_executor_env_policy_mutation_trigger(
        &workspace.path().join(".loopy/loopy.db"),
        "'bogus'",
    )?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected unsupported persisted env policy to fail dispatch");
    assert!(
        error
            .to_string()
            .contains("unsupported executor env_policy bogus"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_config_ref: String = conn.query_row(
        "SELECT executor_config_ref FROM CORE__invocation_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    let executor_config = read_content(&conn, &executor_config_ref)?;
    assert_eq!(executor_config["env_policy"], json!("bogus"));

    Ok(())
}

#[test]
fn start_worker_invocation_rejects_non_string_persisted_env_policy() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = write_stdin_echo_worker_script(workspace.path(), "echo-worker.sh")?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(&install_root, &script_path, &[], 60, None)?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch non-string persisted env policy",
        "dispatch should reject malformed persisted env policies",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    install_executor_env_policy_mutation_trigger(&workspace.path().join(".loopy/loopy.db"), "123")?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected non-string persisted env policy to fail dispatch");
    assert!(
        error
            .to_string()
            .contains("resolved executor config env_policy must be a string"),
        "unexpected error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_config_ref: String = conn.query_row(
        "SELECT executor_config_ref FROM CORE__invocation_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    let executor_config = read_content(&conn, &executor_config_ref)?;
    assert_eq!(executor_config["env_policy"], json!(123));

    Ok(())
}

#[test]
fn start_worker_invocation_does_not_leak_unallowed_parent_environment() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("dump-env.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
python3 -c 'import json, os, sys; print(json.dumps({"home": os.environ.get("HOME"), "openai_api_key": os.environ.get("OPENAI_API_KEY"), "stdin_payload": sys.stdin.read()}))'
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &script_path,
        &[],
        60,
        Some(&["OPENAI_API_KEY".to_owned()]),
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch env isolation",
        "child process should only see allowed env vars",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(invocation.accepted_terminal_api, None);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdout_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stdout'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    let stdout_payload: Value = serde_json::from_str(&stdout_payload_json)?;
    let executor_stdout: Value = serde_json::from_str(
        stdout_payload["text"]
            .as_str()
            .context("missing executor stdout text")?,
    )?;
    assert!(
        executor_stdout["home"].is_null(),
        "HOME leaked into child env: {executor_stdout}"
    );
    assert!(
        executor_stdout["stdin_payload"]
            .as_str()
            .context("missing stdin payload echo")?
            .contains("\"role_prompt_markdown\"")
    );

    Ok(())
}

#[test]
fn start_worker_invocation_preserves_path_for_child_processes() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let fake_bin_dir = workspace.path().join("fakebin");
    fs::create_dir_all(&fake_bin_dir)?;
    let script_path = fake_bin_dir.join("print-env.sh");
    fs::write(
        &script_path,
        "#!/bin/bash\ncat >/dev/null || true\nexec /usr/bin/env\n",
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(&install_root, &script_path, &[], 60, Some(&[]))?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch path preservation",
        "child executors should keep PATH so they can resolve standard tools",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(invocation.accepted_terminal_api, None);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdout_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stdout'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    let stdout_payload: Value = serde_json::from_str(&stdout_payload_json)?;
    let executor_stdout = stdout_payload["text"]
        .as_str()
        .context("missing executor stdout text")?;
    assert!(
        executor_stdout
            .lines()
            .any(|line| line.starts_with("PATH=")),
        "PATH missing from child process environment:\n{executor_stdout}"
    );

    Ok(())
}

#[test]
fn start_worker_invocation_preserves_allowlisted_codex_home_for_nested_codex_executor() -> Result<()>
{
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let fake_bin_dir = workspace.path().join("fakebin");
    fs::create_dir_all(&fake_bin_dir)?;
    let script_path = fake_bin_dir.join("codex");
    fs::write(
        &script_path,
        r#"#!/bin/bash
while IFS= read -r _; do :; done
if [[ -n "${CODEX_HOME:-}" ]]; then
  printf '{"codex_home":"%s"}\n' "$CODEX_HOME"
else
  printf '{"codex_home":null}\n'
fi
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch codex home",
        "nested codex executors should inherit the staged CODEX_HOME",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let codex_home = workspace.path().join("isolated-codex-home");
    fs::create_dir_all(&codex_home)?;
    let output = Command::new(install_root.join("bin/loopy-submit-loop"))
        .args([
            "start-worker-invocation",
            "--loop-id",
            &loop_response.loop_id,
            "--stage",
            "planning",
        ])
        .current_dir(workspace.path())
        .env("PATH", path)
        .env("CODEX_HOME", &codex_home)
        .output()
        .context("failed to run bundled start-worker-invocation command")?;
    if !output.status.success() {
        bail!(
            "bundled start-worker-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let response: Value = serde_json::from_slice(&output.stdout)?;
    let invocation_id = response["invocation_id"]
        .as_str()
        .context("missing invocation_id from bundled start-worker-invocation response")?
        .to_owned();

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdout_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stdout'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation_id],
        |row| row.get(0),
    )?;
    let stdout_payload: Value = serde_json::from_str(&stdout_payload_json)?;
    let executor_stdout: Value = serde_json::from_str(
        stdout_payload["text"]
            .as_str()
            .context("missing executor stdout text")?,
    )?;
    assert_eq!(
        executor_stdout["codex_home"],
        Value::String(codex_home.display().to_string())
    );

    Ok(())
}

#[test]
fn start_worker_invocation_allowlists_shared_loopy_dir_for_nested_codex_executor() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let fake_bin_dir = workspace.path().join("fakebin");
    fs::create_dir_all(&fake_bin_dir)?;
    let script_path = fake_bin_dir.join("codex");
    fs::write(
        &script_path,
        r#"#!/bin/bash
while IFS= read -r _; do :; done
for arg in "$@"; do
  printf 'ARG=%s\n' "$arg"
done
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch codex shared loopy dir",
        "nested codex executors should be able to write the shared loopy database",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(install_root.join("bin/loopy-submit-loop"))
        .args([
            "start-worker-invocation",
            "--loop-id",
            &loop_response.loop_id,
            "--stage",
            "planning",
        ])
        .current_dir(workspace.path())
        .env("PATH", path)
        .output()
        .context("failed to run bundled start-worker-invocation command")?;
    if !output.status.success() {
        bail!(
            "bundled start-worker-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let response: Value = serde_json::from_slice(&output.stdout)?;
    let invocation_id = response["invocation_id"]
        .as_str()
        .context("missing invocation_id from bundled start-worker-invocation response")?
        .to_owned();

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stdout_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stdout'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation_id],
        |row| row.get(0),
    )?;
    let stdout_payload: Value = serde_json::from_str(&stdout_payload_json)?;
    let executor_stdout = stdout_payload["text"]
        .as_str()
        .context("missing executor stdout text")?;
    let expected_shared_loopy_dir = workspace.path().join(".loopy").display().to_string();
    assert!(
        executor_stdout.lines().any(|line| line == "ARG=--add-dir"),
        "expected nested codex invocation to receive --add-dir:\n{executor_stdout}"
    );
    assert!(
        executor_stdout
            .lines()
            .any(|line| line == format!("ARG={expected_shared_loopy_dir}")),
        "expected nested codex invocation to allowlist the shared .loopy directory:\n{executor_stdout}"
    );

    Ok(())
}

#[test]
fn start_worker_invocation_enforces_executor_timeout() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("sleepy-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
sleep 2
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(&install_root, &script_path, &[], 1, None)?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch timeout",
        "hung child should fail instead of blocking forever",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let started_at = Instant::now();
    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected timeout to fail dispatch");
    assert!(
        started_at.elapsed() < Duration::from_secs(2),
        "timeout was not enforced promptly: {:?}",
        started_at.elapsed()
    );
    assert!(
        error.to_string().contains("timed out"),
        "unexpected timeout error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let invocation_id: String = conn.query_row(
        "SELECT invocation_id FROM CORE__invocation_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    let (status, reason): (String, String) = conn.query_row(
        r#"
        SELECT current.status, json_extract(event.payload_json, '$.reason')
        FROM CORE__invocation_current current
        JOIN CORE__events event
          ON event.loop_id = current.loop_id
         AND event.event_name = 'CORE__invocation_failed'
        WHERE current.invocation_id = ?1
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![invocation_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(status, "failed");
    assert!(
        reason.contains("timed out"),
        "unexpected failure summary: {reason}"
    );

    Ok(())
}

#[test]
fn request_timeout_extension_persists_without_consuming_the_terminal_token() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &install_root.join("bin/loopy-submit-loop"),
        &[
            "request-timeout-extension".to_owned(),
            "--invocation-context-path".to_owned(),
            "{invocation_context_path}".to_owned(),
            "--requested-timeout-sec".to_owned(),
            "120".to_owned(),
            "--progress-summary".to_owned(),
            "finished repository scan and checkpoint outline".to_owned(),
            "--rationale".to_owned(),
            "the remaining implementation work is in progress but the default timeout is too short"
                .to_owned(),
        ],
        60,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "timeout extension request",
        "workers should be able to persist advisory timeout extension requests",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    assert_eq!(invocation.accepted_terminal_api, None);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let token_state: String = conn.query_row(
        "SELECT token_state FROM CORE__capability_current WHERE invocation_id = ?1",
        params![invocation.invocation_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(token_state, "available");

    let timeout_request_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__events event
        JOIN CORE__contents content
          ON content.content_ref = json_extract(event.payload_json, '$.request_content_ref')
        WHERE event.loop_id = ?1
          AND event.event_name = 'SUBMIT_LOOP__timeout_extension_requested'
        ORDER BY event.event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    let timeout_request: Value = serde_json::from_str(&timeout_request_json)?;
    assert_eq!(timeout_request["requested_timeout_sec"], json!(120));
    assert_eq!(
        timeout_request["progress_summary"],
        json!("finished repository scan and checkpoint outline")
    );
    assert_eq!(
        timeout_request["rationale"],
        json!(
            "the remaining implementation work is in progress but the default timeout is too short"
        )
    );

    Ok(())
}

#[test]
fn timeout_retry_uses_the_latest_timeout_extension_request() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("timeout-retry-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
set -euo pipefail
invocation_context_path="$1"
bundle_bin="$2"
attempt_file="${invocation_context_path}.attempt"
attempt=0
if [[ -f "$attempt_file" ]]; then
  attempt="$(cat "$attempt_file")"
fi
attempt=$((attempt + 1))
printf '%s' "$attempt" > "$attempt_file"

if [[ "$attempt" -eq 1 ]]; then
  "$bundle_bin" request-timeout-extension --invocation-context-path "$invocation_context_path" --requested-timeout-sec 2 --progress-summary "checkpoint outline drafted and initial dependency scan completed" --rationale "the first timeout estimate no longer covers the remaining plan verification work"
  "$bundle_bin" request-timeout-extension --invocation-context-path "$invocation_context_path" --requested-timeout-sec 4 --progress-summary "checkpoint outline drafted and validation is half complete" --rationale "latest estimate needs enough time to finish and submit"
fi

sleep 3
"$bundle_bin" mock-submit-plan-worker "$invocation_context_path"
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &script_path,
        &[
            "{invocation_context_path}".to_owned(),
            install_root.join("bin/loopy-submit-loop").display().to_string(),
        ],
        1,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "timeout retry latest request",
        "latest timeout-extension request should control the retry timeout",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    assert_eq!(
        invocation.accepted_terminal_api.as_deref(),
        Some("SUBMIT_LOOP__submit_checkpoint_plan")
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let request_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__timeout_extension_requested'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(request_count, 2);

    let latest_timeout_sec: i64 = conn.query_row(
        "SELECT requested_timeout_sec FROM SUBMIT_LOOP__timeout_extension_current WHERE invocation_id = ?1",
        params![invocation.invocation_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(latest_timeout_sec, 4);

    let dispatch_attempts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__dispatch_started'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(dispatch_attempts, 2);

    let latest_plan_revision: i64 = conn.query_row(
        "SELECT latest_submitted_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(latest_plan_revision, 1);

    Ok(())
}

#[test]
fn timeout_retry_stops_after_five_attempts() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("timeout-budget-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
set -euo pipefail
invocation_context_path="$1"
bundle_bin="$2"
attempt_file="${invocation_context_path}.attempt"
attempt=0
if [[ -f "$attempt_file" ]]; then
  attempt="$(cat "$attempt_file")"
fi
attempt=$((attempt + 1))
printf '%s' "$attempt" > "$attempt_file"

requested_timeout_sec=$((attempt + 1))
"$bundle_bin" request-timeout-extension --invocation-context-path "$invocation_context_path" --requested-timeout-sec "$requested_timeout_sec" --progress-summary "attempt $attempt is still making progress" --rationale "need a slightly larger retry budget"
sleep $((requested_timeout_sec + 1))
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &script_path,
        &[
            "{invocation_context_path}".to_owned(),
            install_root.join("bin/loopy-submit-loop").display().to_string(),
        ],
        1,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "timeout retry budget",
        "timeout retries should stop after the fifth attempt",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected timeout retry budget to be exhausted");
    assert!(
        error.to_string().contains("timed out"),
        "unexpected timeout error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let dispatch_attempts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__dispatch_started'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(dispatch_attempts, 5);

    let retry_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__dispatch_retry_scheduled'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(retry_events, 4);

    let latest_timeout_sec: i64 = conn.query_row(
        "SELECT requested_timeout_sec FROM SUBMIT_LOOP__timeout_extension_current ORDER BY updated_at DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(latest_timeout_sec, 6);
    let loop_failed_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__loop_failed'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(loop_failed_events, 1);
    let failure_payload_json: String = conn.query_row(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__loop_failed'
        ORDER BY event_id DESC
        LIMIT 1
        "#,
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let failure_payload: Value = serde_json::from_str(&failure_payload_json)?;
    assert_eq!(
        failure_payload["failure_cause_type"],
        json!("system_failure")
    );
    assert!(
        failure_payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("dispatch retry budget")),
        "unexpected failure payload: {failure_payload:#}"
    );
    let show = runtime.show_loop(ShowLoopRequest {
        loop_id: loop_response.loop_id,
    })?;
    assert_eq!(show.status, "failed");
    assert_eq!(
        show.result.as_ref().map(|result| result.status.as_str()),
        Some("failure")
    );

    Ok(())
}

#[test]
fn request_timeout_extension_rejects_vague_progress_requests() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("timeout-vague-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
set -euo pipefail
invocation_context_path="$1"
bundle_bin="$2"
"$bundle_bin" request-timeout-extension --invocation-context-path "$invocation_context_path" --requested-timeout-sec 2 --progress-summary "still making progress" --rationale "need more time"
sleep 3
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &script_path,
        &[
            "{invocation_context_path}".to_owned(),
            install_root.join("bin/loopy-submit-loop").display().to_string(),
        ],
        1,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "timeout vague progress",
        "vague timeout-extension requests should not trigger a retry",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    assert_eq!(invocation.accepted_terminal_api, None);
    assert_eq!(invocation.terminal_result, None);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_stderr = latest_executor_stderr(&conn, &invocation.invocation_id)?;
    assert!(
        executor_stderr.contains("progress_summary must describe concrete progress"),
        "unexpected timeout request stderr: {executor_stderr}"
    );

    let dispatch_attempts =
        count_loop_events(&conn, &loop_response.loop_id, "CORE__dispatch_started")?;
    assert_eq!(dispatch_attempts, 1);
    let retry_events = count_loop_events(
        &conn,
        &loop_response.loop_id,
        "CORE__dispatch_retry_scheduled",
    )?;
    assert_eq!(retry_events, 0);
    let timeout_request_events = count_loop_events(
        &conn,
        &loop_response.loop_id,
        "SUBMIT_LOOP__timeout_extension_requested",
    )?;
    assert_eq!(timeout_request_events, 0);

    Ok(())
}

#[test]
fn request_timeout_extension_rejects_identical_progress_and_rationale() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("timeout-duplicate-text-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
set -euo pipefail
invocation_context_path="$1"
bundle_bin="$2"
text="completed repository scan and drafted the remaining implementation steps"
"$bundle_bin" request-timeout-extension --invocation-context-path "$invocation_context_path" --requested-timeout-sec 2 --progress-summary "$text" --rationale "$text"
sleep 3
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &script_path,
        &[
            "{invocation_context_path}".to_owned(),
            install_root.join("bin/loopy-submit-loop").display().to_string(),
        ],
        1,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "timeout duplicate rationale",
        "timeout-extension requests should reject duplicated progress and rationale text",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    assert_eq!(invocation.accepted_terminal_api, None);
    assert_eq!(invocation.terminal_result, None);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_stderr = latest_executor_stderr(&conn, &invocation.invocation_id)?;
    assert!(
        executor_stderr.contains("progress_summary and rationale must differ"),
        "unexpected timeout request stderr: {executor_stderr}"
    );

    let dispatch_attempts =
        count_loop_events(&conn, &loop_response.loop_id, "CORE__dispatch_started")?;
    assert_eq!(dispatch_attempts, 1);
    let timeout_request_events = count_loop_events(
        &conn,
        &loop_response.loop_id,
        "SUBMIT_LOOP__timeout_extension_requested",
    )?;
    assert_eq!(timeout_request_events, 0);

    Ok(())
}

#[test]
fn timeout_retry_ignores_disproportionate_extension_requests() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("timeout-disproportionate-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
set -euo pipefail
invocation_context_path="$1"
bundle_bin="$2"
"$bundle_bin" request-timeout-extension --invocation-context-path "$invocation_context_path" --requested-timeout-sec 10 --progress-summary "completed the repository scan and drafted the remaining steps" --rationale "the current timeout is too short for the remaining implementation and verification work"
sleep 3
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(
        &install_root,
        &script_path,
        &[
            "{invocation_context_path}".to_owned(),
            install_root.join("bin/loopy-submit-loop").display().to_string(),
        ],
        1,
        None,
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "timeout disproportionate request",
        "disproportionate timeout requests should not trigger a retry",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id.clone(),
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected disproportionate timeout request to fail without retry");
    assert!(
        error.to_string().contains("timed out"),
        "unexpected timeout error: {error:#}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let dispatch_attempts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__dispatch_started'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(dispatch_attempts, 1);
    let retry_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__dispatch_retry_scheduled'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(retry_events, 0);

    Ok(())
}

#[test]
fn start_worker_invocation_drains_large_stderr_without_timing_out() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let script_path = workspace.path().join("stderr-flood-worker.sh");
    fs::write(
        &script_path,
        r#"#!/bin/bash
python3 - <<'PY'
import os

payload = b"x" * (2 * 1024 * 1024)
written = 0
while written < len(payload):
    written += os.write(2, payload[written:])
PY
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    switch_worker_executor_to_mock(&install_root)?;
    configure_mock_worker_executor(&install_root, &script_path, &[], 1, None)?;

    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "dispatch stderr flood",
        "executor should not deadlock when stderr exceeds pipe capacity",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert_eq!(invocation.accepted_terminal_api, None);
    assert!(invocation.transcript_segment_count > 0);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let stderr_len: i64 = conn.query_row(
        r#"
        SELECT length(json_extract(content.payload_json, '$.text'))
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stderr'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation.invocation_id],
        |row| row.get(0),
    )?;
    assert!(
        stderr_len > 1024 * 1024,
        "expected large stderr transcript, got {stderr_len} bytes"
    );

    Ok(())
}

fn switch_worker_executor_to_mock(install_root: &Path) -> Result<()> {
    let task_type_config_path = install_root.join("roles/coding-task/task-type.toml");
    let config = fs::read_to_string(&task_type_config_path)?;
    let updated = config
        .replace(
            "default_planning_worker = \"codex_planner\"",
            "default_planning_worker = \"mock_planner\"",
        )
        .replace(
            "default_artifact_worker = \"codex_implementer\"",
            "default_artifact_worker = \"mock_implementer\"",
        );
    if updated == config {
        bail!(
            "failed to switch coding-task default worker roles to mock roles in {}",
            task_type_config_path.display()
        );
    }
    fs::write(&task_type_config_path, updated)?;
    Ok(())
}

fn switch_worker_executor_to_submit_plan_mock(install_root: &Path) -> Result<()> {
    let task_type_config_path = install_root.join("roles/coding-task/task-type.toml");
    let config = fs::read_to_string(&task_type_config_path)?;
    let updated_config = config
        .replace(
            "default_planning_worker = \"codex_planner\"",
            "default_planning_worker = \"mock_planner\"",
        )
        .replace(
            "default_artifact_worker = \"codex_implementer\"",
            "default_artifact_worker = \"mock_implementer\"",
        );
    if updated_config == config {
        bail!(
            "failed to switch coding-task default worker roles to mock roles in {}",
            task_type_config_path.display()
        );
    }
    fs::write(&task_type_config_path, updated_config)?;

    let worker_path = install_root.join("roles/coding-task/planning_worker/mock_planner.md");
    let worker = fs::read_to_string(&worker_path)?;
    let worker = worker.replace(
        "executor = \"mock_worker\"",
        "executor = \"mock_submit_plan_worker\"",
    );
    fs::write(&worker_path, worker)?;
    Ok(())
}

fn configure_mock_worker_executor(
    install_root: &Path,
    command: &Path,
    args: &[String],
    timeout_sec: i64,
    env_allow: Option<&[String]>,
) -> Result<()> {
    let manifest_path = install_root.join("submit-loop.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let old_block = r#"[executors.mock_worker]
kind = "local_command"
command = "{bundle_bin}"
args = ["mock-executor", "worker", "{invocation_context_path}"]
cwd = "worktree"
timeout_sec = 60
transcript_capture = "stdio"
"#;
    let mut new_block = format!(
        "[executors.mock_worker]\nkind = \"local_command\"\ncommand = {}\nargs = {}\nbypass_sandbox_args = {}\nbypass_sandbox_inherit_env = true\ncwd = \"worktree\"\ntimeout_sec = {}\ntranscript_capture = \"stdio\"\n",
        serde_json::to_string(&command.display().to_string())?,
        serde_json::to_string(args)?,
        serde_json::to_string(args)?,
        timeout_sec,
    );
    if let Some(env_allow) = env_allow {
        new_block.push_str(&format!(
            "env_allow = {}\n",
            serde_json::to_string(env_allow)?
        ));
    }
    let manifest = manifest.replace(old_block, &new_block);
    fs::write(&manifest_path, manifest)?;
    Ok(())
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

fn invocation_context_path(workspace_root: &Path, invocation_id: &str) -> PathBuf {
    workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"))
}

fn has_nearby_schema_fields(
    haystack: &str,
    anchor: &str,
    required_fields: &[&str],
    radius: usize,
) -> bool {
    haystack.match_indices(anchor).any(|(offset, _)| {
        let start = offset.saturating_sub(radius);
        let end = (offset + anchor.len() + radius).min(haystack.len());
        let window = &haystack[start..end];
        required_fields
            .iter()
            .all(|field| window.contains(field))
    })
}

fn advance_loop_to_artifact_phase(
    runtime: &Runtime,
    workspace_root: &Path,
    loop_id: &str,
) -> Result<(PathBuf, String)> {
    let worktree_path = prepare_loop_worktree(runtime, loop_id)?;
    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_id.to_owned(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(
            workspace_root,
            &planning_worker.invocation_id,
        ),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("advance loop into artifact".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_id.to_owned(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_id.to_owned(),
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(workspace_root, &reviewer.invocation_id),
        submission_id: "checkpoint-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let conn = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC LIMIT 1",
        params![loop_id],
        |row| row.get(0),
    )?;

    Ok((worktree_path, checkpoint_id))
}

fn create_candidate_commit(
    worktree_path: &Path,
    file_name: &str,
    contents: &str,
    message: &str,
) -> Result<String> {
    fs::write(worktree_path.join(file_name), contents)?;

    let git_add = Command::new("git")
        .args(["add", file_name])
        .current_dir(worktree_path)
        .output()
        .context("failed to stage candidate artifact")?;
    if !git_add.status.success() {
        bail!(
            "git add failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&git_add.stdout),
            String::from_utf8_lossy(&git_add.stderr)
        );
    }

    let git_commit = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(worktree_path)
        .output()
        .context("failed to create candidate commit")?;
    if !git_commit.status.success() {
        bail!(
            "git commit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&git_commit.stdout),
            String::from_utf8_lossy(&git_commit.stderr)
        );
    }

    let rev_parse = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree_path)
        .output()
        .context("failed to read candidate commit sha")?;
    if !rev_parse.status.success() {
        bail!(
            "git rev-parse failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&rev_parse.stdout),
            String::from_utf8_lossy(&rev_parse.stderr)
        );
    }

    Ok(String::from_utf8(rev_parse.stdout)?.trim().to_owned())
}

fn install_bundle_into_workspace(workspace_root: &Path) -> Result<PathBuf> {
    let install_root = workspace_root
        .join(".loopy")
        .join("installed-skills")
        .join("loopy-submit-loop");
    install_bundle_at(&install_root)
}

fn load_mock_executor_stdin_payload(conn: &Connection, invocation_id: &str) -> Result<String> {
    let stdout_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stdout'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation_id],
        |row| row.get(0),
    )?;
    let stdout_payload: Value = serde_json::from_str(&stdout_payload_json)?;
    let executor_stdout: Value = serde_json::from_str(
        stdout_payload["text"]
            .as_str()
            .context("missing executor stdout text")?,
    )?;
    Ok(executor_stdout["stdin_payload"]
        .as_str()
        .context("missing stdin_payload echoed by mock executor")?
        .to_owned())
}

fn latest_executor_stderr(conn: &Connection, invocation_id: &str) -> Result<String> {
    let stderr_payload_json: String = conn.query_row(
        r#"
        SELECT content.payload_json
        FROM CORE__transcript_segments segment
        JOIN CORE__contents content ON content.content_ref = segment.content_ref
        WHERE segment.invocation_id = ?1 AND segment.summary = 'executor stderr'
        ORDER BY segment.segment_id DESC
        LIMIT 1
        "#,
        params![invocation_id],
        |row| row.get(0),
    )?;
    let stderr_payload: Value = serde_json::from_str(&stderr_payload_json)?;
    stderr_payload["text"]
        .as_str()
        .map(str::to_owned)
        .context("missing executor stderr text")
}

fn count_loop_events(conn: &Connection, loop_id: &str, event_name: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = ?2",
        params![loop_id, event_name],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn read_content(conn: &Connection, content_ref: &str) -> Result<Value> {
    let payload_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        params![content_ref],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&payload_json)?)
}

fn write_stdin_echo_worker_script(workspace_root: &Path, file_name: &str) -> Result<PathBuf> {
    let script_path = workspace_root.join(file_name);
    fs::write(
        &script_path,
        r#"#!/bin/bash
python3 -c 'import json, sys; print(json.dumps({"stdin_payload": sys.stdin.read()}))'
"#,
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;
    Ok(script_path)
}

fn install_executor_env_policy_mutation_trigger(
    db_path: &Path,
    env_policy_sql: &str,
) -> Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(&format!(
        r#"
        CREATE TRIGGER mutate_executor_env_policy_after_dispatch_started
        AFTER INSERT ON CORE__events
        WHEN NEW.event_name = 'CORE__dispatch_started'
        BEGIN
            UPDATE CORE__contents
               SET payload_json = json_set(payload_json, '$.env_policy', {env_policy_sql})
             WHERE content_ref = (
                 SELECT executor_config_ref
                 FROM CORE__invocation_current
                 WHERE invocation_id = json_extract(NEW.payload_json, '$.invocation_id')
             );
        END;
        "#
    ))?;
    Ok(())
}

fn install_bundle_at(install_root: &Path) -> Result<PathBuf> {
    let repo_root = crate::support::repo_root().as_path();
    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(install_root)
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
    Ok(install_root.to_path_buf())
}

fn open_loop_request(summary: &str, context: &str) -> OpenLoopRequest {
    OpenLoopRequest {
        summary: summary.to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(context.to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    }
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
