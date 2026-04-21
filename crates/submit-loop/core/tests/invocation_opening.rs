mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use loopy::{
    FinalizeFailureRequest, OpenLoopRequest, OpenReviewRoundRequest, PrepareWorktreeRequest,
    ReviewKind, Runtime, StartReviewerInvocationRequest, StartWorkerInvocationRequest,
    SubmitArtifactReviewRequest, SubmitCandidateCommitRequest, SubmitCheckpointPlanRequest,
    SubmitCheckpointReviewRequest, WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::{checkpoint, install_bundle_into_codex_home, with_env_vars};
use tempfile::TempDir;
use toml::Value as TomlValue;

#[test]
fn open_worker_invocation_snapshots_role_executor_and_context_from_installed_bundle() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "plan a loop",
        "need a planning worker invocation",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    assert!(!invocation.invocation_id.is_empty());
    assert!(!invocation.token.is_empty());

    let loop_id = loop_response.loop_id.clone();
    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let opened_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__invocation_opened'",
        [loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(opened_events, 1);

    let (status, stage): (String, String) = conn.query_row(
        "SELECT status, stage FROM CORE__invocation_current WHERE invocation_id = ?1",
        [invocation.invocation_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(status, "failed");
    assert_eq!(stage, "planning");

    let token_state: String = conn.query_row(
        "SELECT token_state FROM CORE__capability_current WHERE invocation_id = ?1",
        [invocation.invocation_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(token_state, "available");

    let context_payload = read_content(&conn, &invocation.invocation_context_ref)?;
    assert_eq!(context_payload["loop_id"], Value::String(loop_id));
    assert_eq!(
        context_payload["stage"],
        Value::String("planning".to_owned())
    );
    assert_eq!(
        context_payload["allowed_terminal_apis"],
        json!([
            "SUBMIT_LOOP__submit_checkpoint_plan",
            "SUBMIT_LOOP__declare_worker_blocked"
        ])
    );
    assert_eq!(
        context_payload["resolved_executor_config_ref"],
        Value::String(invocation.executor_config_ref.clone())
    );
    assert_eq!(
        context_payload["role_definition_ref"],
        Value::String(invocation.role_definition_ref.clone())
    );
    assert_eq!(
        context_payload["token"],
        Value::String(invocation.token.clone())
    );
    assert!(context_payload["loopy_api_contract"].is_object());
    assert!(context_payload["review_history_ref"].as_str().is_some());
    let invocation_context_path = PathBuf::from(
        context_payload["invocation_context_path"]
            .as_str()
            .context("missing invocation_context_path")?,
    );
    assert!(
        invocation_context_path.is_file(),
        "expected invocation context file at {}",
        invocation_context_path.display()
    );
    let persisted_context: Value =
        serde_json::from_str(&fs::read_to_string(&invocation_context_path)?)?;
    assert_eq!(persisted_context, context_payload);

    let role_payload = read_content(&conn, &invocation.role_definition_ref)?;
    assert_eq!(
        role_payload["role"],
        Value::String("planning_worker".to_owned())
    );

    Ok(())
}

#[test]
fn open_worker_invocation_rejects_legacy_worker_role_layout_from_installed_bundle() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    install_legacy_worker_task_type(&install_root)?;
    let runtime = Runtime::new(workspace.path())?;
    let error = runtime
        .open_loop(OpenLoopRequest {
            summary: "legacy worker layout".to_owned(),
            task_type: "legacy-task".to_owned(),
            context: Some("worker/*.md layouts should no longer be accepted".to_owned()),
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: Some(json!({})),
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("legacy worker role layout should be rejected");
    assert!(
        error
            .to_string()
            .contains("planning_worker/legacy_worker.md"),
        "unexpected legacy worker layout error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_worker_invocation_allowlists_shared_loopy_dir_for_codex_executor() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "plan a loop",
        "codex worker should be able to update the shared loopy database",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_config = read_content(&conn, &invocation.executor_config_ref)?;
    let command = executor_config["command"]
        .as_array()
        .context("missing executor command array")?;
    let expected_loopy_dir = workspace.path().join(".loopy").display().to_string();
    let expected_worktree_git_dir = fs::read_to_string(
        workspace
            .path()
            .join(".loopy/worktrees")
            .join(loop_response.label)
            .join(".git"),
    )?
    .trim()
    .strip_prefix("gitdir:")
    .map(str::trim)
    .context("expected worktree .git to contain a gitdir pointer")?
    .to_owned();

    let add_dir_values = command
        .windows(2)
        .filter_map(|window| {
            if window[0] == "--add-dir" {
                window[1].as_str()
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(
        add_dir_values.iter().any(|value| *value == expected_loopy_dir),
        "expected loopy add-dir in command: {:?}",
        command
    );
    assert!(
        add_dir_values
            .iter()
            .any(|value| *value == expected_worktree_git_dir),
        "expected worktree gitdir add-dir in command: {:?}",
        command
    );

    Ok(())
}

#[test]
fn open_worker_invocation_uses_bypass_sandbox_executor_variant() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_bypass(
        "plan a loop",
        "worker invocations should resolve bypass sandbox executor args",
        true,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_config = read_content(&conn, &invocation.executor_config_ref)?;
    assert_bypass_sandbox_executor_variant(&executor_config)?;

    Ok(())
}

#[test]
fn open_reviewer_invocation_uses_bypass_sandbox_executor_variant() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_bypass(
        "review a submitted plan",
        "reviewer invocations should resolve bypass sandbox executor args",
        true,
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
        notes: Some("first reviewable draft".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let invocation = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id,
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let executor_config = read_content(&conn, &invocation.executor_config_ref)?;
    assert_bypass_sandbox_executor_variant(&executor_config)?;

    Ok(())
}

#[test]
fn start_reviewer_invocation_waits_for_a_transient_write_lock() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "review under transient lock",
        "reviewer dispatch should wait for short sqlite write locks",
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
        notes: Some("exercise a short-lived sqlite write lock".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;

    let (lock_started_tx, lock_started_rx) = mpsc::channel();
    let db_path = workspace.path().join(".loopy").join("loopy.db");
    let lock_thread = thread::spawn(move || -> Result<()> {
        let connection = Connection::open(db_path)?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        lock_started_tx
            .send(())
            .expect("main thread should still be waiting for the write lock");
        thread::sleep(Duration::from_millis(250));
        connection.execute_batch("COMMIT")?;
        Ok(())
    });
    lock_started_rx
        .recv()
        .expect("write lock thread should signal after BEGIN IMMEDIATE");

    let reviewer_result = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id,
        review_slot_id: review_round.review_slot_ids[0].clone(),
    });

    lock_thread
        .join()
        .expect("write lock thread should not panic")?;

    let reviewer = match reviewer_result {
        Ok(reviewer) => reviewer,
        Err(error) => {
            let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
            let invocation_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM CORE__invocation_current WHERE loop_id = ?1 AND stage = 'checkpoint_review'",
                [loop_response.loop_id],
                |row| row.get(0),
            )?;
            let journal_mode: String =
                conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            panic!(
                "start_reviewer_invocation failed: {error:#}\ncheckpoint_review_invocations={invocation_count}\njournal_mode={journal_mode}"
            );
        }
    };

    assert!(!reviewer.invocation_id.is_empty());
    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let invocation_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__invocation_current WHERE invocation_id = ?1",
        [reviewer.invocation_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(invocation_count, 1);

    Ok(())
}

#[test]
fn pending_reviewer_slot_can_be_restarted_after_protocol_failure() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "retry failed reviewer slot".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("pending reviewer slots should be restartable after protocol failure".to_owned()),
        planning_worker: Some("mock_planner".to_owned()),
        artifact_worker: Some("mock_implementer".to_owned()),
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
        notes: Some("exercise reviewer restart after protocol failure".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let review_slot_id = review_round.review_slot_ids[0].clone();

    let first = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_slot_id.clone(),
    })?;
    assert_eq!(first.accepted_terminal_api, None);

    let second = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id,
        review_round_id: review_round.review_round_id,
        review_slot_id: review_slot_id.clone(),
    })?;
    assert_eq!(second.accepted_terminal_api, None);
    assert_ne!(first.invocation_id, second.invocation_id);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let failed_invocations_for_slot: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__invocation_current WHERE review_slot_id = ?1 AND status = 'failed'",
        [review_slot_id],
        |row| row.get(0),
    )?;
    assert_eq!(failed_invocations_for_slot, 2);

    Ok(())
}

#[test]
fn open_worker_invocation_rejects_bypass_sandbox_without_manifest_args() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    remove_executor_bypass_sandbox_args(&install_root, "codex_worker")?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_bypass(
        "plan a loop",
        "bypass sandbox invocations should fail if manifest support is missing",
        true,
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected bypass sandbox worker invocation to require manifest args");

    assert!(
        error.to_string().contains("bypass_sandbox_args"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_review_round_derives_slot_count_and_reviewer_role_ids_from_loop_selection() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "bind reviewers".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("bind checkpoint reviewers from loop-open selection".to_owned()),
        planning_worker: Some("mock_planner".to_owned()),
        artifact_worker: Some("mock_implementer".to_owned()),
        checkpoint_reviewers: Some(vec!["codex_scope".to_owned(), "mock".to_owned()]),
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
        notes: None,
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    assert_eq!(review_round.review_slot_ids.len(), 2);

    let second_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[1].clone(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let context_payload = read_content(&conn, &second_reviewer.invocation_context_ref)?;
    assert_eq!(context_payload["reviewer_role_id"], json!("mock"));
    assert_eq!(context_payload["selected_role_id"], json!("mock"));
    assert!(
        context_payload["selected_role_path"]
            .as_str()
            .context("missing selected_role_path")?
            .ends_with("roles/coding-task/checkpoint_reviewer/mock.md")
    );

    Ok(())
}

#[test]
fn legacy_resolved_role_selection_payloads_are_rejected() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "resume legacy loop",
        "legacy resolved role selections should fail after the hard cut",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    overwrite_resolved_role_selection(
        workspace.path(),
        &loop_response.loop_id,
        &json!({
            "task_type": "coding-task",
            "worker": "codex_worker",
            "checkpoint_reviewers": ["default"],
            "artifact_reviewers": ["default"],
        }),
    )?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("legacy resolved role selections should be rejected");
    assert!(
        error.to_string().contains("planning_worker")
            || error.to_string().contains("resolved_role_selection"),
        "unexpected legacy resolved role selection error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_reviewer_invocation_snapshots_review_target_and_runtime_api_contract() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "review a submitted plan",
        "need a checkpoint reviewer invocation",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: workspace
            .path()
            .join(".loopy")
            .join("invocations")
            .join(format!("{}.json", worker.invocation_id)),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A"), checkpoint("Checkpoint B")],
        improvement_opportunities: None,
        notes: Some("first reviewable draft".to_owned()),
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let invocation = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: review_round.review_round_id.clone(),
        review_slot_id: review_round.review_slot_ids[0].clone(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let context_payload = read_content(&conn, &invocation.invocation_context_ref)?;
    assert_eq!(
        context_payload["actor_role"],
        Value::String("reviewer".to_owned())
    );
    assert_eq!(
        context_payload["review_round_id"],
        Value::String(review_round.review_round_id.clone())
    );
    assert_eq!(
        context_payload["review_slot_id"],
        Value::String(review_round.review_slot_ids[0].clone())
    );
    assert_eq!(
        context_payload["review_kind"],
        Value::String("checkpoint".to_owned())
    );
    assert_eq!(
        context_payload["review_target"]["type"],
        Value::String("plan_revision".to_owned())
    );
    assert_eq!(
        context_payload["review_target"]["ref"],
        Value::String("plan-1".to_owned())
    );
    assert_eq!(context_payload["review_target"]["plan_revision"], json!(1));
    let checkpoints = context_payload["review_target"]["checkpoints"]
        .as_array()
        .context("missing checkpoint review target checkpoints")?;
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0]["sequence_index"], json!(0));
    assert_eq!(
        checkpoints[0]["title"],
        Value::String("Checkpoint A".to_owned())
    );
    assert_eq!(checkpoints[0]["kind"], json!("artifact"));
    assert_eq!(checkpoints[0]["deliverables"][0]["type"], json!("file"));
    assert_eq!(
        checkpoints[0]["acceptance"]["verification_steps"][0],
        json!("test -f artifacts/checkpoint-a.txt")
    );
    assert_eq!(
        checkpoints[0]["acceptance"]["expected_outcomes"][0],
        json!("Checkpoint A deliverable is present")
    );
    assert_eq!(checkpoints[1]["sequence_index"], json!(1));
    assert_eq!(
        checkpoints[1]["title"],
        Value::String("Checkpoint B".to_owned())
    );
    assert!(context_payload["loopy_api_contract"].is_object());
    assert_eq!(
        context_payload["allowed_terminal_apis"],
        json!([
            "SUBMIT_LOOP__submit_checkpoint_review",
            "SUBMIT_LOOP__declare_review_blocked"
        ])
    );

    let role_payload = read_content(&conn, &invocation.role_definition_ref)?;
    assert_eq!(
        role_payload["role"],
        Value::String("checkpoint_reviewer".to_owned())
    );

    Ok(())
}

#[test]
fn open_worker_invocation_rejects_artifact_workers_after_loop_failure() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "closed artifact worker",
        "artifact workers must not open after the loop has failed",
        Some(vec!["codex_scope"]),
        None,
    ))?;
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
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: workspace
            .path()
            .join(".loopy")
            .join("invocations")
            .join(format!("{}.json", reviewer.invocation_id)),
        submission_id: "review-1".to_owned(),
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

    runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: loop_response.loop_id.clone(),
        failure_cause_type: "coordinator_failure".to_owned(),
        summary: "closed after plan acceptance".to_owned(),
    })?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Artifact,
            checkpoint_id: Some(checkpoint_id),
        })
        .expect_err("expected artifact workers to be rejected after loop failure");
    assert!(
        error.to_string().contains("status") || error.to_string().contains("failed"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_worker_invocation_rejects_unknown_executor_in_selected_role_front_matter() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "plan a loop",
        "need a planning worker invocation",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let worker_file = install_root.join("roles/coding-task/planning_worker/codex_planner.md");
    let worker_contents = fs::read_to_string(&worker_file)?;
    let mutated = worker_contents.replace(
        "executor = \"codex_worker\"",
        "executor = \"missing_executor\"",
    );
    fs::write(&worker_file, mutated)?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected runtime to reject unknown executor");

    assert!(
        error.to_string().contains("missing executor profile"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_worker_invocation_does_not_write_context_file_before_commit() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "plan a loop",
        "failed invocation open must not leak context files",
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;
    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_invocation_open
        BEFORE INSERT ON CORE__events
        WHEN NEW.event_name = 'CORE__invocation_opened'
        BEGIN
            SELECT RAISE(ABORT, 'forced invocation-open failure');
        END;
        "#,
    )?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected invocation open to fail");
    assert!(
        error.to_string().contains("forced invocation-open failure"),
        "unexpected error: {error:#}"
    );

    let invocations_dir = workspace.path().join(".loopy").join("invocations");
    let leftover_files = if invocations_dir.is_dir() {
        fs::read_dir(&invocations_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    assert!(
        leftover_files.is_empty(),
        "expected no invocation context files after failed open, found {leftover_files:?}"
    );
    let invocation_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__invocation_current", [], |row| {
            row.get(0)
        })?;
    let capability_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__capability_current", [], |row| {
            row.get(0)
        })?;
    assert_eq!(invocation_rows, 0);
    assert_eq!(capability_rows, 0);

    Ok(())
}

#[test]
fn open_worker_invocation_rejects_planning_workers_after_plan_acceptance() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "stale planning worker",
        "planning workers should be rejected once artifact execution begins",
    ))?;
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
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: workspace
            .path()
            .join(".loopy")
            .join("invocations")
            .join(format!("{}.json", reviewer.invocation_id)),
        submission_id: "review-1".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Proceed".to_owned(),
        notes: None,
    })?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected planning worker to be rejected after plan acceptance");
    assert!(
        error.to_string().contains("planning") || error.to_string().contains("artifact"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_worker_invocation_rejects_planning_workers_before_worktree_prepare() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "unprepared planning worker",
        "planning workers should not open before prepare-worktree creates the cwd",
    ))?;

    let error = runtime
        .start_worker_invocation(StartWorkerInvocationRequest {
            loop_id: loop_response.loop_id,
            stage: WorkerStage::Planning,
            checkpoint_id: None,
        })
        .expect_err("expected planning worker opening to be rejected before prepare-worktree");
    assert!(
        error.to_string().contains("prepare-worktree")
            || error.to_string().contains("awaiting_worktree")
            || error.to_string().contains("worktree"),
        "unexpected error: {error:#}"
    );
    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let invocation_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__invocation_current", [], |row| {
            row.get(0)
        })?;
    let capability_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__capability_current", [], |row| {
            row.get(0)
        })?;
    assert_eq!(invocation_rows, 0);
    assert_eq!(capability_rows, 0);

    Ok(())
}

#[test]
fn open_reviewer_invocation_does_not_write_context_file_before_commit() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request(
        "review a submitted plan",
        "failed reviewer invocation open must not leak context files",
    ))?;
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
        notes: None,
    })?;
    let review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let invocation_rows_before: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__invocation_current", [], |row| {
            row.get(0)
        })?;
    let capability_rows_before: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__capability_current", [], |row| {
            row.get(0)
        })?;
    let content_rows_before: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__contents", [], |row| row.get(0))?;
    let invocations_dir = workspace.path().join(".loopy").join("invocations");
    let files_before = if invocations_dir.is_dir() {
        fs::read_dir(&invocations_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_reviewer_invocation_open
        BEFORE INSERT ON CORE__events
        WHEN NEW.event_name = 'CORE__invocation_opened'
          AND json_extract(NEW.payload_json, '$.invocation_role') = 'reviewer'
        BEGIN
            SELECT RAISE(ABORT, 'forced reviewer-invocation-open failure');
        END;
        "#,
    )?;

    let error = runtime
        .start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: loop_response.loop_id,
            review_round_id: review_round.review_round_id,
            review_slot_id: review_round.review_slot_ids[0].clone(),
        })
        .expect_err("expected reviewer invocation open to fail");
    assert!(
        error
            .to_string()
            .contains("forced reviewer-invocation-open failure"),
        "unexpected error: {error:#}"
    );

    let leftover_files = if invocations_dir.is_dir() {
        fs::read_dir(&invocations_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    assert!(
        leftover_files == files_before,
        "expected failed reviewer open to avoid creating new invocation context files; before={files_before:?} after={leftover_files:?}"
    );
    let invocation_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__invocation_current", [], |row| {
            row.get(0)
        })?;
    let capability_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__capability_current", [], |row| {
            row.get(0)
        })?;
    let content_rows_after: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__contents", [], |row| row.get(0))?;
    assert_eq!(invocation_rows, invocation_rows_before);
    assert_eq!(capability_rows, capability_rows_before);
    assert_eq!(content_rows_after, content_rows_before);

    Ok(())
}

#[test]
fn worker_history_uses_latest_five_review_results_and_starts_empty_on_first_attempts() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "plan a loop",
        "first-attempt workers should start with empty review history",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract"]),
    ))?;
    prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let planning_context = read_content(&conn, &planning_worker.invocation_context_ref)?;
    let planning_history_ref = planning_context["review_history_ref"]
        .as_str()
        .context("missing planning review_history_ref")?;
    let planning_history = read_content(&conn, planning_history_ref)?;
    assert_eq!(planning_context["review_history"], planning_history);
    assert_eq!(
        planning_context["review_history"]["latest_result"],
        Value::Null
    );
    assert_eq!(
        planning_context["review_history"]["previous_results"],
        json!([])
    );

    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &planning_worker.invocation_id,
        ),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("initial plan".to_owned()),
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

    let checkpoint_id: String = conn.query_row(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 LIMIT 1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id),
    })?;
    let artifact_context = read_content(&conn, &artifact_worker.invocation_context_ref)?;
    let artifact_history_ref = artifact_context["review_history_ref"]
        .as_str()
        .context("missing artifact review_history_ref")?;
    let artifact_history = read_content(&conn, artifact_history_ref)?;
    assert_eq!(artifact_context["review_history"], artifact_history);
    assert_eq!(
        artifact_context["review_history"]["latest_result"],
        Value::Null
    );
    assert_eq!(
        artifact_context["review_history"]["previous_results"],
        json!([])
    );
    assert_eq!(
        artifact_context["bound_checkpoint"]["title"],
        json!("Checkpoint A")
    );
    assert_eq!(
        artifact_context["bound_checkpoint"]["kind"],
        json!("artifact")
    );
    assert_eq!(
        artifact_context["bound_checkpoint"]["deliverables"][0]["path"],
        json!("artifacts/checkpoint-a.txt")
    );
    assert_eq!(
        artifact_context["bound_checkpoint"]["acceptance"]["verification_steps"][0],
        json!("test -f artifacts/checkpoint-a.txt")
    );

    Ok(())
}

#[test]
fn worker_history_includes_latest_full_checkpoint_result_for_reopened_planning_workers()
-> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "retry a plan",
        "reopened planning workers should receive structured checkpoint review history",
        Some(vec!["codex_scope", "mock"]),
        None,
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
        submission_id: "checkpoint-reject-1".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Define rollback criteria",
            "rationale": "Need explicit rollback criteria.",
            "expected_revision": "Add rollback criteria to the checkpoint plan.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: Some(vec![json!({
            "summary": "Consider a rollback runbook template",
            "rationale": "A standard template would reduce future planning churn.",
            "suggested_follow_up": "Add a reusable rollback-runbook template for future loops.",
        })]),
        summary: "Fix sequencing".to_owned(),
        notes: Some("Need explicit rollback criteria.".to_owned()),
    })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &second_reviewer.invocation_id,
        ),
        submission_id: "checkpoint-reject-2".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "List validation command",
            "rationale": "Spell out the validation command.",
            "expected_revision": "Add the missing validation command to the checkpoint plan.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Add validation".to_owned(),
        notes: Some("Spell out the validation command.".to_owned()),
    })?;

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let context = read_content(&conn, &reopened_worker.invocation_context_ref)?;
    let history_ref = context["review_history_ref"]
        .as_str()
        .context("missing reopened planning review_history_ref")?;
    let history = read_content(&conn, history_ref)?;
    assert_eq!(context["review_history"], history);

    let latest = &context["review_history"]["latest_result"];
    assert_eq!(
        latest["review_round_id"],
        Value::String(review_round.review_round_id)
    );
    assert_eq!(latest["round_status"], Value::String("rejected".to_owned()));
    assert_eq!(
        latest["target_type"],
        Value::String("plan_revision".to_owned())
    );
    assert_eq!(latest["target_ref"], Value::String("plan-1".to_owned()));
    assert_eq!(latest["target_metadata"]["plan_revision"], json!(1));
    assert!(latest.get("improvement_opportunities").is_none());
    assert_eq!(
        latest["blocking_issues"],
        json!([
            {
                "summary": "Define rollback criteria",
                "rationale": "Need explicit rollback criteria.",
                "expected_revision": "Add rollback criteria to the checkpoint plan."
            },
            {
                "summary": "List validation command",
                "rationale": "Spell out the validation command.",
                "expected_revision": "Add the missing validation command to the checkpoint plan."
            }
        ])
    );
    assert_eq!(context["review_history"]["previous_results"], json!([]));

    Ok(())
}

#[test]
fn worker_history_includes_latest_full_artifact_result_for_reopened_artifact_workers() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "retry an artifact",
        "reopened artifact workers should receive structured artifact review history",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract"]),
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
    fs::write(worktree_path.join("feature.txt"), "candidate one\n")?;
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
            "headline": "candidate one",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: Some("first candidate".to_owned()),
    })?;

    let artifact_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: artifact_review_round.review_round_id.clone(),
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-reject-1".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "Exit code semantics are wrong",
            "rationale": "Preserve failure exit semantics in JSON mode.",
            "expected_revision": "Keep the existing failure exit-code behavior in JSON mode.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: Some(vec![json!({
            "summary": "Consider adding a reusable JSON-mode fixture",
            "rationale": "The review found a useful follow-up that is out of scope for this checkpoint.",
            "suggested_follow_up": "Add a fixture that exercises JSON-mode failure semantics across commands.",
        })]),
        summary: "Reject JSON exit code".to_owned(),
        notes: Some("Preserve failure exit semantics in JSON mode.".to_owned()),
    })?;

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;

    let context = read_content(&conn, &reopened_worker.invocation_context_ref)?;
    let history_ref = context["review_history_ref"]
        .as_str()
        .context("missing reopened artifact review_history_ref")?;
    let history = read_content(&conn, history_ref)?;
    assert_eq!(context["review_history"], history);

    let latest = &context["review_history"]["latest_result"];
    assert_eq!(
        latest["review_round_id"],
        Value::String(artifact_review_round.review_round_id)
    );
    assert_eq!(latest["round_status"], Value::String("rejected".to_owned()));
    assert_eq!(
        latest["target_type"],
        Value::String("checkpoint_id".to_owned())
    );
    assert_eq!(latest["target_ref"], Value::String(checkpoint_id.clone()));
    assert_eq!(
        latest["target_metadata"]["checkpoint_id"],
        Value::String(checkpoint_id)
    );
    assert_eq!(
        latest["target_metadata"]["candidate_commit_sha"],
        Value::String(candidate_commit_sha)
    );
    assert!(latest.get("improvement_opportunities").is_none());
    assert_eq!(
        latest["blocking_issues"],
        json!([
            {
                "summary": "Exit code semantics are wrong",
                "rationale": "Preserve failure exit semantics in JSON mode.",
                "expected_revision": "Keep the existing failure exit-code behavior in JSON mode."
            }
        ])
    );
    assert_eq!(context["review_history"]["previous_results"], json!([]));

    Ok(())
}

#[test]
fn worker_history_does_not_reuse_artifact_rejection_after_a_later_approval() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "replace a rejected artifact",
        "later approvals must clear revision guidance for future workers",
        Some(vec!["codex_scope"]),
        Some(vec!["codex_checkpoint_contract"]),
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
    fs::write(worktree_path.join("feature.txt"), "candidate one\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate one"])?;
    let first_candidate_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &first_worker.invocation_id,
        ),
        submission_id: "candidate-1".to_owned(),
        candidate_commit_sha: first_candidate_sha,
        change_summary: json!({
            "headline": "candidate one",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;
    let first_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let first_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: first_review_round.review_round_id,
        review_slot_id: first_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &first_reviewer.invocation_id,
        ),
        submission_id: "artifact-reject-1".to_owned(),
        decision: "reject".to_owned(),
        blocking_issues: vec![json!({
            "summary": "First attempt failed review",
            "rationale": "Fix the first candidate.",
            "expected_revision": "Revise the candidate commit to address the first review failure.",
        })],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Reject first attempt".to_owned(),
        notes: Some("Fix the first candidate.".to_owned()),
    })?;

    let second_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    fs::write(worktree_path.join("feature.txt"), "candidate two\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "candidate two"])?;
    let second_candidate_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &second_worker.invocation_id,
        ),
        submission_id: "candidate-2".to_owned(),
        candidate_commit_sha: second_candidate_sha,
        change_summary: json!({
            "headline": "candidate two",
            "files": ["feature.txt"],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;
    let second_review_round = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: loop_response.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let second_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        review_round_id: second_review_round.review_round_id,
        review_slot_id: second_review_round.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &second_reviewer.invocation_id,
        ),
        submission_id: "artifact-approve-1".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "Approved".to_owned(),
        notes: None,
    })?;

    let reopened_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id,
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id),
    })?;
    let context = read_content(&conn, &reopened_worker.invocation_context_ref)?;
    assert_eq!(context["review_history"]["latest_result"], Value::Null);

    Ok(())
}

#[test]
fn reviewer_history_ignores_other_review_kinds_for_shared_role_ids() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(open_loop_request_with_reviewers(
        "reviewer history kind isolation",
        "shared reviewer ids must not mix checkpoint and artifact history",
        Some(vec!["mock"]),
        Some(vec!["mock"]),
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
    fs::write(worktree_path.join("feature.txt"), "artifact candidate\n")?;
    git(&worktree_path, &["add", "feature.txt"])?;
    git(&worktree_path, &["commit", "-m", "artifact candidate"])?;
    let candidate_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace.path(),
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-submit".to_owned(),
        candidate_commit_sha: candidate_sha,
        change_summary: json!({
            "headline": "artifact candidate",
            "files": ["feature.txt"],
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
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: loop_response.loop_id,
        review_round_id: artifact_review_round.review_round_id,
        review_slot_id: artifact_review_round.review_slot_ids[0].clone(),
    })?;
    let context = read_content(&conn, &artifact_reviewer.invocation_context_ref)?;
    assert_eq!(context["reviewer_history"]["latest_result"], Value::Null);
    assert_eq!(context["reviewer_history"]["previous_results"], json!([]));

    Ok(())
}

#[test]
fn open_worker_invocation_requires_workspace_installed_bundle_root() -> Result<()> {
    let workspace = git_workspace()?;
    let loop_open_runtime = Runtime::with_installed_skill_root(
        workspace.path(),
        crate::support::submit_loop_source_root().as_path(),
    )?;
    let loop_response = loop_open_runtime.open_loop(open_loop_request(
        "plan a loop",
        "worker opens should resolve only from the installed bundle",
    ))?;
    prepare_loop_worktree(&loop_open_runtime, &loop_response.loop_id)?;
    let isolated_home = tempfile::tempdir()?;

    let error = with_env_vars(
        &[("HOME", Some(isolated_home.path())), ("CODEX_HOME", None)],
        || {
            let runtime = Runtime::new(workspace.path())?;
            let error = runtime
                .start_worker_invocation(StartWorkerInvocationRequest {
                    loop_id: loop_response.loop_id.clone(),
                    stage: WorkerStage::Planning,
                    checkpoint_id: None,
                })
                .expect_err("expected missing installed bundle to fail worker invocation opening");
            Ok(error)
        },
    )?;

    assert!(
        error
            .to_string()
            .contains("failed to discover installed skill loopy:submit-loop"),
        "unexpected error: {error:#}"
    );
    assert!(
        !error
            .to_string()
            .contains(".loopy/installed-skills/loopy-submit-loop"),
        "runtime should no longer hard-code the workspace-local installed bundle path: {error:#}"
    );

    Ok(())
}

#[test]
fn open_worker_invocation_uses_host_default_codex_home_bundle_discovery() -> Result<()> {
    let workspace = git_workspace()?;
    let codex_home = tempfile::tempdir()?;
    let isolated_home = tempfile::tempdir()?;
    let install_root = install_bundle_into_codex_home(codex_home.path())?;
    install_fake_codex_command(workspace.path(), &install_root)?;

    with_env_vars(
        &[
            ("HOME", Some(isolated_home.path())),
            ("CODEX_HOME", Some(codex_home.path())),
        ],
        || {
            let runtime = Runtime::new(workspace.path())?;
            let loop_response = runtime.open_loop(open_loop_request(
                "plan a loop",
                "host-default discovery should find the installed submit-loop bundle",
            ))?;
            prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

            let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
                loop_id: loop_response.loop_id,
                stage: WorkerStage::Planning,
                checkpoint_id: None,
            })?;
            assert!(invocation.invocation_id.starts_with("inv-"));

            let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
            let executor_config = read_content(&conn, &invocation.executor_config_ref)?;
            let command = executor_config["command"]
                .as_array()
                .context("missing executor command array")?;
            let add_dir_values = command
                .windows(2)
                .filter_map(|window| {
                    (window.first()? == "--add-dir").then(|| window.get(1)?.as_str())
                })
                .collect::<Option<Vec<_>>>()
                .context("expected --add-dir values in codex executor command")?;
            let expected_loopy_dir = workspace.path().join(".loopy").display().to_string();
            let expected_codex_home = codex_home.path().display().to_string();
            assert!(
                add_dir_values
                    .iter()
                    .any(|value| *value == expected_loopy_dir),
                "expected shared .loopy add-dir in command: {:?}",
                add_dir_values
            );
            assert!(
                add_dir_values
                    .iter()
                    .any(|value| *value == expected_codex_home),
                "expected CODEX_HOME add-dir in command: {:?}",
                add_dir_values
            );

            Ok(())
        },
    )?;

    Ok(())
}

#[test]
fn open_worker_invocation_falls_back_to_installed_bundle_with_unrelated_dev_registry() -> Result<()>
{
    let workspace = git_workspace()?;
    write_unrelated_dev_registry(workspace.path())?;
    let codex_home = tempfile::tempdir()?;
    let isolated_home = tempfile::tempdir()?;
    let install_root = install_bundle_into_codex_home(codex_home.path())?;
    install_fake_codex_command(workspace.path(), &install_root)?;

    with_env_vars(
        &[
            ("HOME", Some(isolated_home.path())),
            ("CODEX_HOME", Some(codex_home.path())),
        ],
        || {
            let runtime = Runtime::new(workspace.path())?;
            let loop_response = runtime.open_loop(open_loop_request(
                "plan a loop",
                "unrelated dev registries must not block installed bundle discovery",
            ))?;
            prepare_loop_worktree(&runtime, &loop_response.loop_id)?;

            let invocation = runtime.start_worker_invocation(StartWorkerInvocationRequest {
                loop_id: loop_response.loop_id,
                stage: WorkerStage::Planning,
                checkpoint_id: None,
            })?;
            assert!(invocation.invocation_id.starts_with("inv-"));

            Ok(())
        },
    )?;

    Ok(())
}

fn install_bundle_into_workspace(workspace_root: &Path) -> Result<PathBuf> {
    let install_root = workspace_root
        .join(".loopy")
        .join("installed-skills")
        .join("loopy-submit-loop");
    let install_root = install_bundle_at(&install_root)?;
    crate::support::write_submit_loop_dev_registry(workspace_root, &install_root)?;
    install_fake_codex_command(workspace_root, &install_root)?;
    Ok(install_root)
}

fn write_unrelated_dev_registry(workspace_root: &Path) -> Result<()> {
    let registry_dir = workspace_root.join("skills");
    fs::create_dir_all(&registry_dir)?;
    fs::write(
        registry_dir.join("dev-registry.toml"),
        [
            "[[skills]]",
            "skill_id = \"loopy:other-skill\"",
            "loader_id = \"loopy.other.v1\"",
            "source_root = \"other-skill\"",
            "binary_package = \"loopy-other-skill\"",
            "binary_name = \"loopy-other-skill\"",
            "internal_manifest = \"other-skill.toml\"",
            "",
        ]
        .join("\n"),
    )?;
    Ok(())
}

fn install_legacy_worker_task_type(install_root: &Path) -> Result<()> {
    let task_root = install_root.join("roles/legacy-task");
    fs::create_dir_all(task_root.join("worker"))?;
    fs::create_dir_all(task_root.join("checkpoint_reviewer"))?;
    fs::create_dir_all(task_root.join("artifact_reviewer"))?;
    fs::write(
        task_root.join("task-type.toml"),
        [
            "task_type = \"legacy-task\"",
            "default_planning_worker = \"legacy_worker\"",
            "default_artifact_worker = \"legacy_worker\"",
            "default_checkpoint_reviewers = [\"mock\"]",
            "default_artifact_reviewers = [\"mock\"]",
            "",
        ]
        .join("\n"),
    )?;
    fs::write(
        task_root.join("worker/legacy_worker.md"),
        [
            "---",
            "role = \"worker\"",
            "executor = \"mock_worker\"",
            "---",
            "",
            "# Legacy Worker",
            "",
            "Use this legacy worker only in compatibility tests.",
            "",
        ]
        .join("\n"),
    )?;
    fs::write(
        task_root.join("checkpoint_reviewer/mock.md"),
        [
            "---",
            "role = \"checkpoint_reviewer\"",
            "executor = \"mock_checkpoint_reviewer\"",
            "---",
            "",
            "# Mock Checkpoint Reviewer",
            "",
            "Use this deterministic reviewer in compatibility tests.",
            "",
        ]
        .join("\n"),
    )?;
    fs::write(
        task_root.join("artifact_reviewer/mock.md"),
        [
            "---",
            "role = \"artifact_reviewer\"",
            "executor = \"mock_artifact_reviewer\"",
            "---",
            "",
            "# Mock Artifact Reviewer",
            "",
            "Use this deterministic reviewer in compatibility tests.",
            "",
        ]
        .join("\n"),
    )?;
    Ok(())
}

fn remove_executor_bypass_sandbox_args(install_root: &Path, executor_name: &str) -> Result<()> {
    let manifest_path = install_root.join("submit-loop.toml");
    let manifest: String = fs::read_to_string(&manifest_path)?;
    let mut manifest: TomlValue = toml::from_str(&manifest)?;
    let executor = manifest
        .get_mut("executors")
        .and_then(TomlValue::as_table_mut)
        .and_then(|executors| executors.get_mut(executor_name))
        .and_then(TomlValue::as_table_mut)
        .with_context(|| format!("missing executor profile {executor_name}"))?;
    executor.remove("bypass_sandbox_args");
    fs::write(&manifest_path, toml::to_string(&manifest)?)?;
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

fn open_loop_request_with_bypass(
    summary: &str,
    context: &str,
    bypass_sandbox: bool,
) -> OpenLoopRequest {
    let mut request = open_loop_request(summary, context);
    request.bypass_sandbox = Some(bypass_sandbox);
    request
}

fn invocation_context_path(workspace_root: &Path, invocation_id: &str) -> PathBuf {
    workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"))
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

fn read_content(conn: &Connection, content_ref: &str) -> Result<Value> {
    let payload_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        params![content_ref],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&payload_json)?)
}

fn overwrite_resolved_role_selection(
    workspace_root: &Path,
    loop_id: &str,
    payload: &Value,
) -> Result<()> {
    let conn = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    conn.execute(
        r#"
        UPDATE CORE__contents
           SET payload_json = ?1
         WHERE content_ref = (
             SELECT resolved_role_selection_ref
             FROM SUBMIT_LOOP__loop_current
             WHERE loop_id = ?2
         )
        "#,
        params![serde_json::to_string(payload)?, loop_id],
    )?;
    Ok(())
}

fn assert_bypass_sandbox_executor_variant(executor_config: &Value) -> Result<()> {
    let command = executor_config["command"]
        .as_array()
        .context("missing executor command array")?;
    let command = command
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("executor command entries must be strings"))
        })
        .collect::<Result<Vec<_>>>()?;

    assert!(
        command
            .iter()
            .any(|entry| entry == "--dangerously-bypass-approvals-and-sandbox"),
        "expected bypass sandbox flag in command: {command:?}"
    );
    assert!(
        !command.iter().any(|entry| entry == "--full-auto"),
        "did not expect --full-auto in bypass sandbox command: {command:?}"
    );
    assert_eq!(executor_config["bypass_sandbox"], json!(true));
    assert_eq!(executor_config["args_variant"], json!("bypass_sandbox"));
    assert_eq!(executor_config["env_policy"], json!("inherit_all"));

    Ok(())
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
