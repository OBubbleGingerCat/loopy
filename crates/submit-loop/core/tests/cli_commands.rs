mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use loopy::{
    BeginCallerFinalizeRequest, BlockCallerFinalizeRequest, HandoffToCallerFinalizeRequest,
    OpenLoopRequest, OpenReviewRoundRequest, PrepareWorktreeRequest, ReviewKind, Runtime,
    StartReviewerInvocationRequest, StartWorkerInvocationRequest, WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::{accept_single_checkpoint_loop, checkpoint_json};
use tempfile::TempDir;

#[test]
fn bundled_cli_prepares_worktree_and_starts_worker_invocation_from_the_installed_skill_root()
-> Result<()> {
    let install_root = install_bundle()?;
    switch_worker_executor_to_mock(&install_root)?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "exercise bundled cli",
            "--task-type",
            "coding-task",
            "--context",
            "verify installed bundle execution",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled open-loop command")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }
    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();
    let worktree_branch = open_loop_json["branch"]
        .as_str()
        .context("missing branch")?
        .to_owned();
    let db_path = open_loop_json["db_path"]
        .as_str()
        .context("missing db_path")?;
    assert_eq!(
        Path::new(db_path),
        workspace.path().join(".loopy/loopy.db").as_path()
    );

    let prepare_output = Command::new(&bundled_loopy)
        .args(["prepare-worktree", "--loop-id", &loop_id])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled prepare-worktree command")?;
    if !prepare_output.status.success() {
        bail!(
            "prepare-worktree failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&prepare_output.stdout),
            String::from_utf8_lossy(&prepare_output.stderr)
        );
    }
    let prepare_json: Value = serde_json::from_slice(&prepare_output.stdout)?;
    assert_eq!(prepare_json["loop_id"].as_str(), Some(loop_id.as_str()));
    assert_eq!(
        prepare_json["branch"].as_str(),
        Some(worktree_branch.as_str())
    );
    assert!(prepare_json["path"].as_str().is_some());
    assert_eq!(prepare_json["lifecycle"].as_str(), Some("prepared"));

    let worker_output = Command::new(&bundled_loopy)
        .args([
            "start-worker-invocation",
            "--loop-id",
            &loop_id,
            "--stage",
            "planning",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled start-worker-invocation command")?;
    if !worker_output.status.success() {
        bail!(
            "start-worker-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&worker_output.stdout),
            String::from_utf8_lossy(&worker_output.stderr)
        );
    }
    let worker_json: Value = serde_json::from_slice(&worker_output.stdout)?;
    worker_json["invocation_id"]
        .as_str()
        .context("missing invocation_id")?;
    assert!(worker_json["invocation_id"].as_str().is_some());
    assert!(worker_json["accepted_terminal_api"].is_null());
    assert!(
        worker_json["transcript_segment_count"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );

    Ok(())
}

#[test]
fn bundled_cli_open_loop_accepts_task_type_and_defaults_optional_context() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "exercise bundled cli",
            "--task-type",
            "coding-task",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled open-loop command")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }
    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let loop_input_ref: String = conn.query_row(
        "SELECT loop_input_ref FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![&loop_id],
        |row| row.get(0),
    )?;
    let payload_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        params![loop_input_ref],
        |row| row.get(0),
    )?;
    let loop_input: Value = serde_json::from_str(&payload_json)?;

    assert_eq!(
        loop_input["summary"],
        Value::String("exercise bundled cli".into())
    );
    assert_eq!(loop_input["task_type"], Value::String("coding-task".into()));
    assert_eq!(loop_input["context"], Value::String(String::new()));
    assert_eq!(loop_input["bypass_sandbox"], Value::Bool(false));

    Ok(())
}

#[test]
fn bundled_cli_open_loop_rejects_legacy_worker_flag_and_default_reviewer_ids() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "legacy bundled cli",
            "--task-type",
            "coding-task",
            "--worker",
            "mock_worker",
            "--checkpoint-reviewers-json",
            "[\"default\"]",
            "--artifact-reviewers-json",
            "[\"default\"]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled legacy open-loop command")?;
    assert!(
        !open_loop_output.status.success(),
        "open-loop unexpectedly accepted legacy worker flag\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&open_loop_output.stdout),
        String::from_utf8_lossy(&open_loop_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&open_loop_output.stderr);
    assert_eq!(
        stderr.contains("--worker"),
        true,
        "stderr should mention the removed --worker flag: {stderr}"
    );

    Ok(())
}

#[test]
fn bundled_cli_submit_checkpoint_review_rejects_legacy_issues_json_and_optional_summary()
-> Result<()> {
    let install_root = install_bundle()?;
    switch_worker_executor_to_mock(&install_root)?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    let runtime = Runtime::with_installed_skill_root(workspace.path(), &install_root)?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "legacy checkpoint review cli".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("legacy review flags should stay compatible".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let planning_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", planning_worker.invocation_id));
    let submit_plan_output = Command::new(&bundled_loopy)
        .args([
            "submit-checkpoint-plan",
            "--invocation-context-path",
            planning_context_path
                .to_str()
                .context("non-utf8 planning context path")?,
            "--submission-id",
            "plan-submit",
            "--checkpoints-json",
            &checkpoint_json("Checkpoint A"),
            "--improvement-opportunities-json",
            "[]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to submit checkpoint plan for legacy review cli test")?;
    if !submit_plan_output.status.success() {
        bail!(
            "submit-checkpoint-plan failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&submit_plan_output.stdout),
            String::from_utf8_lossy(&submit_plan_output.stderr)
        );
    }
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
    let reviewer_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", reviewer.invocation_id));
    let legacy_review_output = Command::new(&bundled_loopy)
        .args([
            "submit-checkpoint-review",
            "--invocation-context-path",
            reviewer_context_path
                .to_str()
                .context("non-utf8 reviewer context path")?,
            "--submission-id",
            "checkpoint-reject",
            "--decision",
            "reject",
            "--issues-json",
            "[\"Legacy blocker\"]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to submit legacy checkpoint review")?;
    assert_eq!(
        !legacy_review_output.status.success(),
        true,
        "submit-checkpoint-review unexpectedly accepted legacy --issues-json\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&legacy_review_output.stdout),
        String::from_utf8_lossy(&legacy_review_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&legacy_review_output.stderr);
    assert!(
        stderr.contains("--issues-json"),
        "stderr should mention the removed --issues-json flag: {stderr}"
    );

    Ok(())
}

#[test]
fn bundled_cli_submit_checkpoint_review_rejects_legacy_approve_issues_json() -> Result<()> {
    let install_root = install_bundle()?;
    switch_worker_executor_to_mock(&install_root)?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    let runtime = Runtime::with_installed_skill_root(workspace.path(), &install_root)?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "legacy approve issues-json".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "legacy approve reviews with advisory comments should stay compatible".to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let worker_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", worker.invocation_id));
    let submit_plan_output = Command::new(&bundled_loopy)
        .args([
            "submit-checkpoint-plan",
            "--invocation-context-path",
            worker_context_path
                .to_str()
                .context("non-utf8 worker context path")?,
            "--submission-id",
            "plan-submit",
            "--checkpoints-json",
            &checkpoint_json("Checkpoint A"),
            "--improvement-opportunities-json",
            "[]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to submit checkpoint plan for legacy approve review cli test")?;
    if !submit_plan_output.status.success() {
        bail!(
            "submit-checkpoint-plan failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&submit_plan_output.stdout),
            String::from_utf8_lossy(&submit_plan_output.stderr)
        );
    }
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
    let reviewer_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", reviewer.invocation_id));
    let legacy_review_output = Command::new(&bundled_loopy)
        .args([
            "submit-checkpoint-review",
            "--invocation-context-path",
            reviewer_context_path
                .to_str()
                .context("non-utf8 reviewer context path")?,
            "--submission-id",
            "checkpoint-approve",
            "--decision",
            "approve",
            "--issues-json",
            "[\"Legacy advisory\"]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to submit legacy approved checkpoint review")?;
    assert_eq!(
        !legacy_review_output.status.success(),
        true,
        "submit-checkpoint-review unexpectedly accepted legacy approve --issues-json\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&legacy_review_output.stdout),
        String::from_utf8_lossy(&legacy_review_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&legacy_review_output.stderr);
    assert!(
        stderr.contains("--issues-json"),
        "stderr should mention the removed --issues-json flag: {stderr}"
    );

    Ok(())
}

#[test]
fn bundled_cli_declare_worker_blocked_rejects_legacy_flags() -> Result<()> {
    let install_root = install_bundle()?;
    switch_worker_executor_to_mock(&install_root)?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    let runtime = Runtime::with_installed_skill_root(workspace.path(), &install_root)?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "legacy worker blocked cli".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("legacy blocked flags should stay compatible".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let worker_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", worker.invocation_id));
    let legacy_blocked_output = Command::new(&bundled_loopy)
        .args([
            "declare-worker-blocked",
            "--invocation-context-path",
            worker_context_path
                .to_str()
                .context("non-utf8 worker context path")?,
            "--submission-id",
            "worker-blocked",
            "--reason",
            "Missing build dependency",
            "--blocking-type",
            "environment",
            "--suggested-next-action",
            "Install the missing dependency and rerun the worker",
            "--notes",
            "legacy blocked path",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to declare worker blocked with legacy flags")?;
    assert_eq!(
        !legacy_blocked_output.status.success(),
        true,
        "declare-worker-blocked unexpectedly accepted legacy flags\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&legacy_blocked_output.stdout),
        String::from_utf8_lossy(&legacy_blocked_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&legacy_blocked_output.stderr);
    assert!(
        stderr.contains("--reason")
            || stderr.contains("--blocking-type")
            || stderr.contains("--suggested-next-action"),
        "stderr should mention the removed legacy blocked flags: {stderr}"
    );

    Ok(())
}

#[test]
fn bundled_cli_declare_review_blocked_rejects_legacy_flags() -> Result<()> {
    let install_root = install_bundle()?;
    switch_worker_executor_to_mock(&install_root)?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    let runtime = Runtime::with_installed_skill_root(workspace.path(), &install_root)?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "legacy review blocked cli".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("legacy reviewer blocked flags should stay compatible".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let planning_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: loop_response.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    let planning_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", planning_worker.invocation_id));
    let submit_plan_output = Command::new(&bundled_loopy)
        .args([
            "submit-checkpoint-plan",
            "--invocation-context-path",
            planning_context_path
                .to_str()
                .context("non-utf8 planning context path")?,
            "--submission-id",
            "plan-submit",
            "--checkpoints-json",
            &checkpoint_json("Checkpoint A"),
            "--improvement-opportunities-json",
            "[]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to submit checkpoint plan for legacy review-blocked cli test")?;
    if !submit_plan_output.status.success() {
        bail!(
            "submit-checkpoint-plan failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&submit_plan_output.stdout),
            String::from_utf8_lossy(&submit_plan_output.stderr)
        );
    }
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
    let reviewer_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{}.json", reviewer.invocation_id));
    let legacy_blocked_output = Command::new(&bundled_loopy)
        .args([
            "declare-review-blocked",
            "--invocation-context-path",
            reviewer_context_path
                .to_str()
                .context("non-utf8 reviewer context path")?,
            "--submission-id",
            "review-blocked",
            "--reason",
            "External review tool is unavailable",
            "--blocking-type",
            "dependency",
            "--suggested-next-action",
            "Restore the review tool and reopen the reviewer invocation",
            "--notes",
            "legacy review blocked path",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to declare review blocked with legacy flags")?;
    assert_eq!(
        !legacy_blocked_output.status.success(),
        true,
        "declare-review-blocked unexpectedly accepted legacy flags\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&legacy_blocked_output.stdout),
        String::from_utf8_lossy(&legacy_blocked_output.stderr)
    );
    let stderr = String::from_utf8_lossy(&legacy_blocked_output.stderr);
    assert!(
        stderr.contains("--reason")
            || stderr.contains("--blocking-type")
            || stderr.contains("--suggested-next-action"),
        "stderr should mention the removed legacy blocked flags: {stderr}"
    );

    Ok(())
}

#[test]
fn bundled_cli_open_loop_persists_bypass_sandbox_and_show_loop_reports_it() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "bypass sandbox loop",
            "--task-type",
            "coding-task",
            "--bypass-sandbox",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled open-loop command")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }
    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let loop_input_ref: String = conn.query_row(
        "SELECT loop_input_ref FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_id],
        |row| row.get(0),
    )?;
    let payload_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        params![loop_input_ref],
        |row| row.get(0),
    )?;
    let loop_input: Value = serde_json::from_str(&payload_json)?;
    assert_eq!(loop_input["bypass_sandbox"], Value::Bool(true));

    let show_loop_output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &loop_id, "--json"])
        .current_dir(workspace.path())
        .output()
        .context("failed to render show-loop json output")?;
    if !show_loop_output.status.success() {
        bail!(
            "show-loop --json failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&show_loop_output.stdout),
            String::from_utf8_lossy(&show_loop_output.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&show_loop_output.stdout)?;
    assert_eq!(payload["bypass_sandbox"], Value::Bool(true));

    let table_output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &loop_id])
        .current_dir(workspace.path())
        .output()
        .context("failed to render show-loop table output")?;
    if !table_output.status.success() {
        bail!(
            "show-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&table_output.stdout),
            String::from_utf8_lossy(&table_output.stderr)
        );
    }
    let table_text = String::from_utf8(table_output.stdout)?;
    assert_eq!(table_row_value(&table_text, "bypass_sandbox")?, "true");

    Ok(())
}

#[test]
fn bundled_cli_show_loop_requires_explicit_loop_id() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let output = Command::new(&bundled_loopy)
        .args(["show-loop"])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled show-loop command")?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--loop-id"),
        "stderr should mention the required --loop-id flag"
    );

    Ok(())
}

#[test]
fn bundled_cli_show_loop_reports_in_progress_loop_state() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "show current loop",
            "--task-type",
            "coding-task",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to open loop for show-loop test")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }

    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();

    let table_output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &loop_id])
        .current_dir(workspace.path())
        .output()
        .context("failed to render show-loop table output")?;
    if !table_output.status.success() {
        bail!(
            "show-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&table_output.stdout),
            String::from_utf8_lossy(&table_output.stderr)
        );
    }
    let table_text = String::from_utf8(table_output.stdout)?;
    assert!(table_text.contains("loop_id"));
    assert!(table_text.contains(loop_id.as_str()));
    assert!(table_text.contains("status"));
    assert!(table_text.contains("phase"));
    assert!(table_text.contains("updated_at"));
    assert_eq!(table_row_value(&table_text, "bypass_sandbox")?, "false");
    assert!(table_text.contains("plan.latest_submitted"));
    assert!(table_text.contains("plan.executable"));
    assert!(!table_text.contains("plan.review_status"));
    assert!(table_text.contains("worktree.label"));
    assert!(table_text.contains("worktree.branch"));
    assert!(table_text.contains("worktree.lifecycle"));
    assert!(table_text.contains("latest_invocation.role"));
    assert!(table_text.contains("latest_invocation.stage"));
    assert!(table_text.contains("latest_invocation.status"));
    assert!(table_text.contains("latest_invocation.updated_at"));
    assert!(table_text.contains("latest_review.kind"));
    assert!(table_text.contains("latest_review.status"));
    assert!(table_text.contains("latest_review.target_type"));
    assert!(table_text.contains("latest_review.target_ref"));
    assert!(table_text.contains("result.status"));
    assert!(table_text.contains("result.generated_at"));

    let json_output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &loop_id, "--json"])
        .current_dir(workspace.path())
        .output()
        .context("failed to render show-loop json output")?;
    if !json_output.status.success() {
        bail!(
            "show-loop --json failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&json_output.stdout),
            String::from_utf8_lossy(&json_output.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(payload["loop_id"].as_str(), Some(loop_id.as_str()));
    assert!(payload["status"].as_str().is_some());
    assert!(payload["phase"].as_str().is_some());
    assert!(payload["updated_at"].as_str().is_some());
    assert_eq!(payload["bypass_sandbox"], Value::Bool(false));
    assert!(payload["worktree"]["label"].as_str().is_some());
    assert_eq!(payload["result"], Value::Null);

    Ok(())
}

#[test]
fn bundled_cli_show_loop_can_target_workspace_explicitly_from_outside_repo_root() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    let outside_dir = tempfile::tempdir()?;

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "show loop from outside cwd",
            "--task-type",
            "coding-task",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to open loop for explicit workspace show-loop test")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }
    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();

    let output = Command::new(&bundled_loopy)
        .args([
            "show-loop",
            "--loop-id",
            &loop_id,
            "--workspace",
            workspace
                .path()
                .to_str()
                .context("non-utf8 workspace path")?,
            "--json",
        ])
        .current_dir(outside_dir.path())
        .output()
        .context("failed to run bundled show-loop command from outside workspace root")?;

    if !output.status.success() {
        bail!(
            "show-loop --workspace failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(payload["loop_id"].as_str(), Some(loop_id.as_str()));
    assert_eq!(
        payload["worktree"]["path"].as_str(),
        Some(
            workspace
                .path()
                .join(".loopy")
                .join("worktrees")
                .join(
                    open_loop_json["label"]
                        .as_str()
                        .context("missing label in open-loop response")?
                )
                .to_str()
                .context("non-utf8 worktree path")?
        )
    );

    Ok(())
}

#[test]
fn bundled_cli_show_loop_includes_terminal_result_for_completed_loop() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let completed_loop = drive_loop_to_success_for_cli_test(workspace.path(), &bundled_loopy)?;

    let output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &completed_loop.loop_id, "--json"])
        .current_dir(workspace.path())
        .output()
        .context("failed to query completed loop state")?;
    if !output.status.success() {
        bail!(
            "show-loop --json failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        payload["loop_id"].as_str(),
        Some(completed_loop.loop_id.as_str())
    );
    assert_eq!(
        payload["latest_invocation"],
        completed_loop.latest_invocation
    );
    assert_eq!(payload["latest_review"], completed_loop.latest_review);
    assert_eq!(payload["result"]["status"].as_str(), Some("success"));
    assert!(payload["result"]["generated_at"].as_str().is_some());

    Ok(())
}

#[test]
fn bundled_cli_show_loop_uses_event_recency_when_review_updated_at_is_blank() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let completed_loop = drive_loop_to_success_for_cli_test(workspace.path(), &bundled_loopy)?;
    blank_review_current_updated_at_for_cli_test(workspace.path(), &completed_loop.loop_id)?;

    let output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &completed_loop.loop_id, "--json"])
        .current_dir(workspace.path())
        .output()
        .context("failed to query partially migrated completed loop state")?;
    if !output.status.success() {
        bail!(
            "show-loop --json failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        payload["loop_id"].as_str(),
        Some(completed_loop.loop_id.as_str())
    );
    assert_eq!(payload["latest_review"], completed_loop.latest_review);

    Ok(())
}

#[test]
fn bundled_cli_show_loop_rejects_unknown_loop_id() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", "loop-does-not-exist"])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled show-loop command")?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown loop_id"),
        "stderr should report the missing loop id"
    );

    Ok(())
}

#[test]
fn bundled_cli_show_loop_does_not_rebuild_missing_projection_rows() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "show loop should stay read only",
            "--task-type",
            "coding-task",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to open loop for show-loop read-only test")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }

    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();

    let db_path = workspace.path().join(".loopy/loopy.db");
    let conn = Connection::open(&db_path)?;
    conn.execute(
        "DELETE FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_id],
    )?;

    let output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &loop_id])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled show-loop command")?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown loop_id"),
        "stderr should report the missing loop id"
    );

    let remaining_projection_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(remaining_projection_rows, 0);

    Ok(())
}

#[test]
fn bundled_cli_open_review_round_no_longer_accepts_reviewer_count() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let output = Command::new(&bundled_loopy)
        .args([
            "open-review-round",
            "--loop-id",
            "loop-test",
            "--review-kind",
            "checkpoint",
            "--target-type",
            "plan_revision",
            "--target-ref",
            "plan-1",
            "--reviewer-count",
            "2",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled open-review-round with legacy reviewer-count flag")?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--reviewer-count"),
        "stderr should mention the removed reviewer-count flag"
    );

    Ok(())
}

#[test]
fn bundled_cli_exposes_worktree_review_and_success_commands() -> Result<()> {
    let install_root = install_bundle()?;
    switch_worker_executor_to_mock(&install_root)?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "coordinator api cli",
            "--task-type",
            "coding-task",
            "--context",
            "exercise worktree and review round commands",
            "--checkpoint-reviewers-json",
            "[\"mock\"]",
            "--artifact-reviewers-json",
            "[\"mock\"]",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled open-loop command")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }
    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();

    let worktree_created_output = Command::new(&bundled_loopy)
        .args(["prepare-worktree", "--loop-id", &loop_id])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled prepare-worktree command")?;
    if !worktree_created_output.status.success() {
        bail!(
            "prepare-worktree failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&worktree_created_output.stdout),
            String::from_utf8_lossy(&worktree_created_output.stderr)
        );
    }

    let planning_worker_output = Command::new(&bundled_loopy)
        .args([
            "start-worker-invocation",
            "--loop-id",
            &loop_id,
            "--stage",
            "planning",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled start-worker-invocation command")?;
    if !planning_worker_output.status.success() {
        bail!(
            "start-worker-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&planning_worker_output.stdout),
            String::from_utf8_lossy(&planning_worker_output.stderr)
        );
    }
    let planning_worker_json: Value = serde_json::from_slice(&planning_worker_output.stdout)?;
    let planning_invocation_id = planning_worker_json["invocation_id"]
        .as_str()
        .context("missing planning invocation_id")?;
    let invocation_context_path = workspace
        .path()
        .join(".loopy")
        .join("invocations")
        .join(format!("{planning_invocation_id}.json"));

    let submit_plan_output = Command::new(&bundled_loopy)
        .args([
            "submit-checkpoint-plan",
            "--invocation-context-path",
            invocation_context_path
                .to_str()
                .context("non-utf8 invocation context path")?,
            "--submission-id",
            "plan-submit",
            "--checkpoints-json",
            &checkpoint_json("Checkpoint A"),
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled submit-checkpoint-plan command")?;
    if !submit_plan_output.status.success() {
        bail!(
            "submit-checkpoint-plan failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&submit_plan_output.stdout),
            String::from_utf8_lossy(&submit_plan_output.stderr)
        );
    }

    let review_round_output = Command::new(&bundled_loopy)
        .args([
            "open-review-round",
            "--loop-id",
            &loop_id,
            "--review-kind",
            "checkpoint",
            "--target-type",
            "plan_revision",
            "--target-ref",
            "plan-1",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled open-review-round command")?;
    if !review_round_output.status.success() {
        bail!(
            "open-review-round failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&review_round_output.stdout),
            String::from_utf8_lossy(&review_round_output.stderr)
        );
    }
    let review_round_json: Value = serde_json::from_slice(&review_round_output.stdout)?;
    let review_round_id = review_round_json["review_round_id"]
        .as_str()
        .context("missing review_round_id")?
        .to_owned();
    let review_slot_id = review_round_json["review_slot_ids"][0]
        .as_str()
        .context("missing review_slot_id")?
        .to_owned();

    let reviewer_output = Command::new(&bundled_loopy)
        .args([
            "start-reviewer-invocation",
            "--loop-id",
            &loop_id,
            "--review-round-id",
            &review_round_id,
            "--review-slot-id",
            &review_slot_id,
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled start-reviewer-invocation command")?;
    if !reviewer_output.status.success() {
        bail!(
            "start-reviewer-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&reviewer_output.stdout),
            String::from_utf8_lossy(&reviewer_output.stderr)
        );
    }
    let reviewer_json: Value = serde_json::from_slice(&reviewer_output.stdout)?;
    assert!(reviewer_json["invocation_id"].as_str().is_some());
    assert!(reviewer_json["token"].as_str().is_some());

    let current_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let integration_summary_json = json!({
        "strategy": "cherry_pick",
        "landed_commit_shas": [current_head],
        "resolution_notes": Value::Null,
    })
    .to_string();
    let success_output = Command::new(&bundled_loopy)
        .args([
            "finalize-success",
            "--loop-id",
            &loop_id,
            "--integration-summary-json",
            &integration_summary_json,
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled finalize-success command")?;
    assert!(
        !success_output.status.success(),
        "expected finalize-success to reject loops outside caller-finalizing success flow"
    );
    let stderr = String::from_utf8_lossy(&success_output.stderr);
    assert!(
        stderr.contains("cannot finalize success from loop phase"),
        "unexpected stderr: {stderr}"
    );

    Ok(())
}

#[test]
fn bundled_cli_rejects_removed_split_runtime_subcommands() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    for subcommand in [
        "record-worktree-created",
        "record-worktree-create-failed",
        "open-worker-invocation",
        "open-reviewer-invocation",
        "dispatch-invocation",
        "record-worktree-deleted",
        "record-worktree-cleanup-warning",
        "fail-loop",
        "build-failure-result",
        "build-success-result",
        "integrate-accepted-commits",
    ] {
        let output = Command::new(&bundled_loopy)
            .args([subcommand, "--loop-id", "loop-ignored"])
            .current_dir(workspace.path())
            .output()
            .with_context(|| format!("failed to invoke removed subcommand {subcommand}"))?;
        assert!(
            !output.status.success(),
            "expected removed subcommand {subcommand} to be unavailable"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unrecognized subcommand"),
            "expected clap to reject {subcommand}, stderr was: {stderr}"
        );
    }

    Ok(())
}

#[test]
fn show_loop_json_reports_blocked_caller_finalize_context() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    switch_worker_executor_to_mock(&install_root)?;
    let runtime = Runtime::with_installed_skill_root(workspace.path(), &install_root)?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "blocked caller finalize cli",
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
        strategy_summary: "Attempted cherry-pick onto main".to_owned(),
        blocking_summary: "Conflict requires a human decision".to_owned(),
        human_question: "Should the loop version replace the seed line?".to_owned(),
        conflicting_files: vec!["README.md".to_owned()],
        notes: None,
        has_in_progress_integration: true,
    })?;

    let output = Command::new(&bundled_loopy)
        .args(["show-loop", "--loop-id", &accepted.loop_id, "--json"])
        .current_dir(workspace.path())
        .output()
        .context("failed to render blocked show-loop json output")?;
    if !output.status.success() {
        bail!(
            "show-loop --json failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        payload["phase"],
        Value::String("caller_blocked_on_human".to_owned())
    );
    assert_eq!(
        payload["caller_finalize"]["status"],
        Value::String("blocked".to_owned())
    );
    assert_eq!(
        payload["caller_finalize"]["human_question"],
        Value::String("Should the loop version replace the seed line?".to_owned())
    );

    Ok(())
}

#[test]
fn bundled_cli_can_integrate_accepted_commits_back_into_the_caller_branch() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy-submit-loop");

    let completed_loop = drive_loop_to_success_for_cli_test(workspace.path(), &bundled_loopy)?;
    let feature_contents = fs::read_to_string(workspace.path().join("feature.txt"))
        .context("expected feature.txt to be present in the caller worktree after integration")?;
    assert_eq!(feature_contents, "implemented\n");
    assert_eq!(
        completed_loop.success_result["status"].as_str(),
        Some("success")
    );
    let commit_summary = completed_loop.success_result["commit_summary"]
        .as_array()
        .context("success result missing commit_summary array")?;
    assert_eq!(
        commit_summary.len(),
        completed_loop.candidate_commit_shas.len()
    );
    for candidate_commit_sha in &completed_loop.candidate_commit_shas {
        assert!(
            commit_summary.iter().any(|commit| {
                commit["commit_sha"].as_str() == Some(candidate_commit_sha.as_str())
            }),
            "missing accepted commit {candidate_commit_sha} in success result"
        );
    }

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

fn table_row_value<'a>(table: &'a str, field: &str) -> Result<&'a str> {
    table
        .lines()
        .find_map(|line| {
            line.strip_prefix(field).and_then(|rest| {
                let value = rest.trim();
                (!value.is_empty()).then_some(value)
            })
        })
        .with_context(|| format!("missing table row {field} in output:\n{table}"))
}

struct CompletedLoopFixture {
    loop_id: String,
    candidate_commit_shas: Vec<String>,
    success_result: Value,
    latest_invocation: Value,
    latest_review: Value,
}

fn drive_loop_to_success_for_cli_test(
    workspace_root: &Path,
    bundled_loopy: &Path,
) -> Result<CompletedLoopFixture> {
    let install_root = bundled_loopy
        .parent()
        .and_then(Path::parent)
        .context("bundled loopy path should live under <install_root>/bin/loopy-submit-loop")?;
    switch_worker_executor_to_mock(install_root)?;
    let open_loop_output = Command::new(bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "completed show-loop",
            "--task-type",
            "coding-task",
            "--context",
            "exercise completed show-loop output",
            "--checkpoint-reviewers-json",
            "[\"mock\"]",
            "--artifact-reviewers-json",
            "[\"mock\"]",
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to run bundled open-loop command")?;
    if !open_loop_output.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop_output.stdout),
            String::from_utf8_lossy(&open_loop_output.stderr)
        );
    }
    let open_loop_json: Value = serde_json::from_slice(&open_loop_output.stdout)?;
    let loop_id = open_loop_json["loop_id"]
        .as_str()
        .context("missing loop_id")?
        .to_owned();
    let prepare_worktree_output = Command::new(bundled_loopy)
        .args(["prepare-worktree", "--loop-id", &loop_id])
        .current_dir(workspace_root)
        .output()
        .context("failed to prepare loop worktree")?;
    if !prepare_worktree_output.status.success() {
        bail!(
            "prepare-worktree failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&prepare_worktree_output.stdout),
            String::from_utf8_lossy(&prepare_worktree_output.stderr)
        );
    }
    let prepare_worktree_json: Value = serde_json::from_slice(&prepare_worktree_output.stdout)?;
    let worktree_path = PathBuf::from(
        prepare_worktree_json["path"]
            .as_str()
            .context("prepare-worktree response missing path")?,
    );

    let planning_worker_output = Command::new(bundled_loopy)
        .args([
            "start-worker-invocation",
            "--loop-id",
            &loop_id,
            "--stage",
            "planning",
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to start planning worker")?;
    if !planning_worker_output.status.success() {
        bail!(
            "start-worker-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&planning_worker_output.stdout),
            String::from_utf8_lossy(&planning_worker_output.stderr)
        );
    }
    let planning_worker_json: Value = serde_json::from_slice(&planning_worker_output.stdout)?;
    let planning_invocation_id = planning_worker_json["invocation_id"]
        .as_str()
        .context("missing planning invocation_id")?;
    let planning_context_path = workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{planning_invocation_id}.json"));

    let submit_plan_output = Command::new(bundled_loopy)
        .args([
            "submit-checkpoint-plan",
            "--invocation-context-path",
            planning_context_path
                .to_str()
                .context("non-utf8 planning invocation path")?,
            "--submission-id",
            "plan-submit",
            "--checkpoints-json",
            &checkpoint_json("Checkpoint A"),
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to submit checkpoint plan")?;
    if !submit_plan_output.status.success() {
        bail!(
            "submit-checkpoint-plan failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&submit_plan_output.stdout),
            String::from_utf8_lossy(&submit_plan_output.stderr)
        );
    }

    let checkpoint_review_output = Command::new(bundled_loopy)
        .args([
            "open-review-round",
            "--loop-id",
            &loop_id,
            "--review-kind",
            "checkpoint",
            "--target-type",
            "plan_revision",
            "--target-ref",
            "plan-1",
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to open checkpoint review round")?;
    if !checkpoint_review_output.status.success() {
        bail!(
            "open-review-round failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&checkpoint_review_output.stdout),
            String::from_utf8_lossy(&checkpoint_review_output.stderr)
        );
    }
    let checkpoint_review_json: Value = serde_json::from_slice(&checkpoint_review_output.stdout)?;
    let checkpoint_review_round_id = checkpoint_review_json["review_round_id"]
        .as_str()
        .context("missing checkpoint review round id")?;
    let checkpoint_review_slot_id = checkpoint_review_json["review_slot_ids"][0]
        .as_str()
        .context("missing checkpoint review slot id")?;

    let checkpoint_reviewer_output = Command::new(bundled_loopy)
        .args([
            "start-reviewer-invocation",
            "--loop-id",
            &loop_id,
            "--review-round-id",
            checkpoint_review_round_id,
            "--review-slot-id",
            checkpoint_review_slot_id,
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to start checkpoint reviewer invocation")?;
    if !checkpoint_reviewer_output.status.success() {
        bail!(
            "start-reviewer-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&checkpoint_reviewer_output.stdout),
            String::from_utf8_lossy(&checkpoint_reviewer_output.stderr)
        );
    }
    let checkpoint_reviewer_json: Value =
        serde_json::from_slice(&checkpoint_reviewer_output.stdout)?;
    let checkpoint_reviewer_invocation_id = checkpoint_reviewer_json["invocation_id"]
        .as_str()
        .context("missing checkpoint reviewer invocation id")?;
    let checkpoint_reviewer_context_path = workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{checkpoint_reviewer_invocation_id}.json"));

    let checkpoint_approve_output = Command::new(bundled_loopy)
        .args([
            "submit-checkpoint-review",
            "--invocation-context-path",
            checkpoint_reviewer_context_path
                .to_str()
                .context("non-utf8 checkpoint reviewer context path")?,
            "--submission-id",
            "checkpoint-approve",
            "--decision",
            "approve",
            "--blocking-issues-json",
            "[]",
            "--nonblocking-issues-json",
            "[]",
            "--improvement-opportunities-json",
            "[]",
            "--summary",
            "approved",
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to approve checkpoint review")?;
    if !checkpoint_approve_output.status.success() {
        bail!(
            "submit-checkpoint-review failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&checkpoint_approve_output.stdout),
            String::from_utf8_lossy(&checkpoint_approve_output.stderr)
        );
    }

    let checkpoint_ids = load_checkpoint_ids_for_cli_test(workspace_root, &loop_id)?;
    let [checkpoint_id] = checkpoint_ids.as_slice() else {
        bail!(
            "expected exactly one checkpoint in the accepted plan, found {}",
            checkpoint_ids.len()
        );
    };

    let candidate_commit_sha = submit_candidate_commit_for_checkpoint_cli_test(
        workspace_root,
        bundled_loopy,
        &worktree_path,
        &loop_id,
        checkpoint_id,
        "feature.txt",
        "implemented\n",
        "Implement checkpoint A",
        "candidate-submit",
        "Implemented checkpoint A",
    )?;
    let (artifact_review_round_id, artifact_review_slot_ids) =
        open_artifact_review_round_for_cli_test(
            workspace_root,
            bundled_loopy,
            &loop_id,
            checkpoint_id,
        )?;
    let [artifact_review_slot_id] = artifact_review_slot_ids.as_slice() else {
        bail!(
            "expected one artifact review slot for {}, found {}",
            checkpoint_id,
            artifact_review_slot_ids.len()
        );
    };
    let final_invocation_id = submit_artifact_review_approval_for_cli_test(
        workspace_root,
        bundled_loopy,
        &loop_id,
        &artifact_review_round_id,
        artifact_review_slot_id,
        "artifact-approve",
    )?;

    let handoff_output = Command::new(bundled_loopy)
        .args(["handoff-to-caller-finalize", "--loop-id", &loop_id])
        .current_dir(workspace_root)
        .output()
        .context("failed to hand off caller finalize")?;
    if !handoff_output.status.success() {
        bail!(
            "handoff-to-caller-finalize failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&handoff_output.stdout),
            String::from_utf8_lossy(&handoff_output.stderr)
        );
    }
    let begin_output = Command::new(bundled_loopy)
        .args(["begin-caller-finalize", "--loop-id", &loop_id])
        .current_dir(workspace_root)
        .output()
        .context("failed to begin caller finalize")?;
    if !begin_output.status.success() {
        bail!(
            "begin-caller-finalize failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&begin_output.stdout),
            String::from_utf8_lossy(&begin_output.stderr)
        );
    }
    git(workspace_root, &["cherry-pick", &candidate_commit_sha])?;
    let landed_head = git_output(workspace_root, &["rev-parse", "HEAD"])?;
    let integration_summary_json = json!({
        "strategy": "cherry_pick",
        "landed_commit_shas": [landed_head],
        "resolution_notes": "Replay accepted artifact onto caller branch",
    })
    .to_string();

    let success_output = Command::new(bundled_loopy)
        .args([
            "finalize-success",
            "--loop-id",
            &loop_id,
            "--integration-summary-json",
            &integration_summary_json,
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to finalize success result")?;
    if !success_output.status.success() {
        bail!(
            "finalize-success failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&success_output.stdout),
            String::from_utf8_lossy(&success_output.stderr)
        );
    }
    let success_result: Value = serde_json::from_slice(&success_output.stdout)?;
    // Make the earlier checkpoint review round the most recently updated review.
    restamp_review_round_recorded_at_for_cli_test(
        workspace_root,
        &loop_id,
        checkpoint_review_round_id,
        "2099-01-01T00:00:00Z",
    )?;
    rebuild_loop_projections_for_cli_test(workspace_root, bundled_loopy, &loop_id)?;
    let conn = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    let latest_invocation =
        load_show_loop_invocation_snapshot_for_cli_test(&conn, &final_invocation_id)?;
    let latest_review =
        load_show_loop_review_snapshot_for_cli_test(&conn, &loop_id, checkpoint_review_round_id)?;

    Ok(CompletedLoopFixture {
        loop_id,
        candidate_commit_shas: vec![candidate_commit_sha],
        success_result,
        latest_invocation,
        latest_review,
    })
}

fn load_checkpoint_ids_for_cli_test(workspace_root: &Path, loop_id: &str) -> Result<Vec<String>> {
    let conn = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    let mut statement = conn.prepare(
        "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1 ORDER BY sequence_index ASC",
    )?;
    let checkpoint_ids = statement
        .query_map(params![loop_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(checkpoint_ids)
}

fn restamp_review_round_recorded_at_for_cli_test(
    workspace_root: &Path,
    loop_id: &str,
    review_round_id: &str,
    recorded_at: &str,
) -> Result<()> {
    let conn = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    let mut statement = conn.prepare(
        r#"
        SELECT event_id, payload_json
        FROM CORE__events
        WHERE loop_id = ?1
          AND event_name IN (
              'SUBMIT_LOOP__checkpoint_review_round_recorded',
              'SUBMIT_LOOP__artifact_review_round_recorded'
          )
        ORDER BY event_id ASC
        "#,
    )?;
    let events = statement
        .query_map(params![loop_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let event_id = events
        .into_iter()
        .find_map(|(event_id, payload_json)| {
            let payload: Value = serde_json::from_str(&payload_json).ok()?;
            (payload["review_round_id"].as_str() == Some(review_round_id)).then_some(event_id)
        })
        .context("failed to locate recorded review-round event for cli test")?;
    conn.execute(
        "UPDATE CORE__events SET occurred_at = ?2, recorded_at = ?2 WHERE event_id = ?1",
        params![event_id, recorded_at],
    )?;
    Ok(())
}

fn blank_review_current_updated_at_for_cli_test(
    workspace_root: &Path,
    loop_id: &str,
) -> Result<()> {
    let conn = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    conn.execute(
        "UPDATE SUBMIT_LOOP__review_current SET updated_at = '' WHERE loop_id = ?1",
        params![loop_id],
    )?;
    Ok(())
}

fn rebuild_loop_projections_for_cli_test(
    workspace_root: &Path,
    bundled_loopy: &Path,
    loop_id: &str,
) -> Result<()> {
    let rebuild_output = Command::new(bundled_loopy)
        .args(["rebuild-projections", "--loop-id", loop_id])
        .current_dir(workspace_root)
        .output()
        .context("failed to rebuild loop projections for cli test")?;
    if !rebuild_output.status.success() {
        bail!(
            "rebuild-projections failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&rebuild_output.stdout),
            String::from_utf8_lossy(&rebuild_output.stderr)
        );
    }
    Ok(())
}

fn submit_candidate_commit_for_checkpoint_cli_test(
    workspace_root: &Path,
    bundled_loopy: &Path,
    worktree_path: &Path,
    loop_id: &str,
    checkpoint_id: &str,
    file_name: &str,
    file_contents: &str,
    commit_message: &str,
    submission_id: &str,
    headline: &str,
) -> Result<String> {
    let artifact_worker_output = Command::new(bundled_loopy)
        .args([
            "start-worker-invocation",
            "--loop-id",
            loop_id,
            "--stage",
            "artifact",
            "--checkpoint-id",
            checkpoint_id,
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to start artifact worker")?;
    if !artifact_worker_output.status.success() {
        bail!(
            "start-worker-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&artifact_worker_output.stdout),
            String::from_utf8_lossy(&artifact_worker_output.stderr)
        );
    }
    let artifact_worker_json: Value = serde_json::from_slice(&artifact_worker_output.stdout)?;
    let artifact_invocation_id = artifact_worker_json["invocation_id"]
        .as_str()
        .context("missing artifact invocation id")?;
    let artifact_context_path = workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{artifact_invocation_id}.json"));

    fs::write(worktree_path.join(file_name), file_contents)?;
    let git_add = Command::new("git")
        .args(["add", file_name])
        .current_dir(worktree_path)
        .output()
        .with_context(|| format!("failed to git add {file_name}"))?;
    if !git_add.status.success() {
        bail!(
            "git add failed\n{}",
            String::from_utf8_lossy(&git_add.stderr)
        );
    }
    let git_commit = Command::new("git")
        .args(["commit", "-m", commit_message])
        .current_dir(worktree_path)
        .output()
        .with_context(|| format!("failed to commit {file_name}"))?;
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
    let candidate_commit_sha = String::from_utf8(rev_parse.stdout)?.trim().to_owned();
    let change_summary = json!({
        "headline": headline,
        "files": [file_name],
    })
    .to_string();

    let submit_candidate_output = Command::new(bundled_loopy)
        .args([
            "submit-candidate-commit",
            "--invocation-context-path",
            artifact_context_path
                .to_str()
                .context("non-utf8 artifact invocation path")?,
            "--submission-id",
            submission_id,
            "--candidate-commit-sha",
            &candidate_commit_sha,
            "--change-summary-json",
            &change_summary,
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to submit candidate commit")?;
    if !submit_candidate_output.status.success() {
        bail!(
            "submit-candidate-commit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&submit_candidate_output.stdout),
            String::from_utf8_lossy(&submit_candidate_output.stderr)
        );
    }

    Ok(candidate_commit_sha)
}

fn open_artifact_review_round_for_cli_test(
    workspace_root: &Path,
    bundled_loopy: &Path,
    loop_id: &str,
    checkpoint_id: &str,
) -> Result<(String, Vec<String>)> {
    let artifact_review_output = Command::new(bundled_loopy)
        .args([
            "open-review-round",
            "--loop-id",
            loop_id,
            "--review-kind",
            "artifact",
            "--target-type",
            "checkpoint_id",
            "--target-ref",
            checkpoint_id,
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to open artifact review round")?;
    if !artifact_review_output.status.success() {
        bail!(
            "open-review-round failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&artifact_review_output.stdout),
            String::from_utf8_lossy(&artifact_review_output.stderr)
        );
    }
    let artifact_review_json: Value = serde_json::from_slice(&artifact_review_output.stdout)?;
    let review_round_id = artifact_review_json["review_round_id"]
        .as_str()
        .context("missing artifact review round id")?
        .to_owned();
    let review_slot_ids = artifact_review_json["review_slot_ids"]
        .as_array()
        .context("missing artifact review slot ids")?
        .iter()
        .map(|slot| {
            slot.as_str()
                .map(str::to_owned)
                .context("artifact review slot id should be a string")
        })
        .collect::<Result<Vec<_>>>()?;

    Ok((review_round_id, review_slot_ids))
}

fn submit_artifact_review_approval_for_cli_test(
    workspace_root: &Path,
    bundled_loopy: &Path,
    loop_id: &str,
    review_round_id: &str,
    review_slot_id: &str,
    submission_id: &str,
) -> Result<String> {
    let artifact_reviewer_output = Command::new(bundled_loopy)
        .args([
            "start-reviewer-invocation",
            "--loop-id",
            loop_id,
            "--review-round-id",
            review_round_id,
            "--review-slot-id",
            review_slot_id,
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to start artifact reviewer")?;
    if !artifact_reviewer_output.status.success() {
        bail!(
            "start-reviewer-invocation failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&artifact_reviewer_output.stdout),
            String::from_utf8_lossy(&artifact_reviewer_output.stderr)
        );
    }
    let artifact_reviewer_json: Value = serde_json::from_slice(&artifact_reviewer_output.stdout)?;
    let artifact_reviewer_invocation_id = artifact_reviewer_json["invocation_id"]
        .as_str()
        .context("missing artifact reviewer invocation id")?
        .to_owned();
    let artifact_reviewer_context_path = workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{artifact_reviewer_invocation_id}.json"));

    let artifact_approve_output = Command::new(bundled_loopy)
        .args([
            "submit-artifact-review",
            "--invocation-context-path",
            artifact_reviewer_context_path
                .to_str()
                .context("non-utf8 artifact reviewer context path")?,
            "--submission-id",
            submission_id,
            "--decision",
            "approve",
            "--blocking-issues-json",
            "[]",
            "--nonblocking-issues-json",
            "[]",
            "--improvement-opportunities-json",
            "[]",
            "--summary",
            "approved",
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to approve artifact review")?;
    if !artifact_approve_output.status.success() {
        bail!(
            "submit-artifact-review failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&artifact_approve_output.stdout),
            String::from_utf8_lossy(&artifact_approve_output.stderr)
        );
    }

    Ok(artifact_reviewer_invocation_id)
}

fn load_show_loop_invocation_snapshot_for_cli_test(
    conn: &Connection,
    invocation_id: &str,
) -> Result<Value> {
    conn.query_row(
        r#"
        SELECT invocation_id,
               invocation_role,
               stage,
               status,
               accepted_api,
               review_round_id,
               updated_at
        FROM CORE__invocation_current
        WHERE invocation_id = ?1
        "#,
        params![invocation_id],
        |row| {
            Ok(json!({
                "invocation_id": row.get::<_, String>(0)?,
                "invocation_role": row.get::<_, String>(1)?,
                "stage": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "accepted_api": row.get::<_, Option<String>>(4)?,
                "review_round_id": row.get::<_, Option<String>>(5)?,
                "updated_at": row.get::<_, String>(6)?,
            }))
        },
    )
    .context("failed to load final invocation snapshot for cli test")
}

fn load_show_loop_review_snapshot_for_cli_test(
    conn: &Connection,
    loop_id: &str,
    review_round_id: &str,
) -> Result<Value> {
    conn.query_row(
        r#"
        SELECT review_round_id, review_kind, round_status, target_type, target_ref
        FROM SUBMIT_LOOP__review_current
        WHERE loop_id = ?1 AND review_round_id = ?2
        "#,
        params![loop_id, review_round_id],
        |row| {
            Ok(json!({
                "review_round_id": row.get::<_, String>(0)?,
                "review_kind": row.get::<_, String>(1)?,
                "round_status": row.get::<_, String>(2)?,
                "target_type": row.get::<_, String>(3)?,
                "target_ref": row.get::<_, String>(4)?,
            }))
        },
    )
    .context("failed to load final review snapshot for cli test")
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

fn install_bundle() -> Result<PathBuf> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");
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
    Ok(install_base.keep().join("submit-loop-skill"))
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
