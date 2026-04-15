mod support;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use loopy::{
    BeginCallerFinalizeRequest, BlockCallerFinalizeRequest, CallerIntegrationSummary,
    FinalizeFailureRequest, FinalizeSuccessRequest, HandoffToCallerFinalizeRequest,
    OpenLoopRequest, OpenReviewRoundRequest, PrepareWorktreeRequest, ReviewKind, Runtime,
    ShowLoopRequest, StartWorkerInvocationRequest, SubmitCheckpointPlanRequest, WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::Value;
use serde_json::json;
use support::{
    accept_single_checkpoint_loop, checkpoint, git, materialize_loop_worktree_with_mirrored_gitdir,
};
use tempfile::TempDir;

#[test]
fn rebuild_loop_projections_restores_deleted_current_rows_for_one_loop() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "recover loop projections".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("single-loop rebuild should restore deleted current rows".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
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

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    conn.execute(
        "DELETE FROM CORE__capability_current WHERE invocation_id = ?1",
        params![worker.invocation_id.clone()],
    )?;
    conn.execute(
        "DELETE FROM CORE__invocation_current WHERE invocation_id = ?1",
        params![worker.invocation_id.clone()],
    )?;
    conn.execute(
        "DELETE FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
    )?;

    runtime.rebuild_loop_projections(&loop_response.loop_id)?;

    let restored_loop_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let restored_invocation_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__invocation_current WHERE invocation_id = ?1",
        params![worker.invocation_id],
        |row| row.get(0),
    )?;
    let restored_capability_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__capability_current WHERE invocation_id = ?1",
        params![worker.invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(restored_loop_rows, 1);
    assert_eq!(restored_invocation_rows, 1);
    assert_eq!(restored_capability_rows, 1);

    Ok(())
}

#[test]
fn prepare_worktree_adopts_existing_out_of_band_worktree() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "adopt existing worktree".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("prepare-worktree should adopt an already-created worktree".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    let worktree_path = workspace
        .path()
        .join(".loopy")
        .join("worktrees")
        .join(&loop_response.label);
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let head_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace.path())
        .output()
        .context("failed to resolve HEAD before out-of-band worktree creation")?;
    if !head_output.status.success() {
        bail!(
            "git rev-parse HEAD failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&head_output.stdout),
            String::from_utf8_lossy(&head_output.stderr)
        );
    }
    let base_commit_sha = String::from_utf8(head_output.stdout)?.trim().to_owned();
    let add_output = Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &loop_response.branch,
            worktree_path
                .to_str()
                .context("non-utf8 adopted worktree path")?,
            &base_commit_sha,
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to create out-of-band worktree before prepare-worktree retry")?;
    if !add_output.status.success() {
        bail!(
            "git worktree add failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&add_output.stdout),
            String::from_utf8_lossy(&add_output.stderr)
        );
    }

    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

    assert_eq!(
        prepared["loop_id"].as_str(),
        Some(loop_response.loop_id.as_str())
    );
    assert_eq!(
        prepared["branch"].as_str(),
        Some(loop_response.branch.as_str())
    );
    assert_eq!(prepared["lifecycle"].as_str(), Some("prepared"));
    assert_eq!(
        prepared["path"].as_str(),
        Some(
            worktree_path
                .to_str()
                .context("non-utf8 adopted worktree path")?
        )
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let prepared_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__worktree_prepared'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(prepared_event_count, 1);

    Ok(())
}

#[test]
fn caller_finalize_block_can_resume_after_restart() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let accepted = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "resume blocked caller finalize",
        "artifacts/blocked.txt",
        "loop change\n",
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
        blocking_summary: "Conflict requires human decision".to_owned(),
        human_question: "Should the loop version replace the seed line?".to_owned(),
        conflicting_files: vec!["README.md".to_owned()],
        notes: None,
        has_in_progress_integration: true,
    })?;

    let resumed_runtime = Runtime::new(workspace.path())?;
    let blocked = resumed_runtime.show_loop(ShowLoopRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    assert_eq!(blocked.phase, "caller_blocked_on_human");
    assert_eq!(
        blocked
            .caller_finalize
            .as_ref()
            .and_then(|summary| summary.human_question.as_deref()),
        Some("Should the loop version replace the seed line?")
    );

    resumed_runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: accepted.loop_id.clone(),
    })?;
    git(
        workspace.path(),
        &["cherry-pick", &accepted.accepted_commit_sha],
    )?;
    let landed_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let result = resumed_runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: accepted.loop_id,
        integration_summary: CallerIntegrationSummary {
            strategy: "manual_resolution".to_owned(),
            landed_commit_shas: vec![landed_head],
            resolution_notes: Some("Human chose the loop version".to_owned()),
        },
    })?;
    assert_eq!(result["status"], Value::String("success".to_owned()));

    Ok(())
}

#[test]
fn second_ready_loop_can_finalize_after_first_loop_advances_head() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let first = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "first loop",
        "artifacts/first.txt",
        "first\n",
    )?;
    let second = accept_single_checkpoint_loop(
        &runtime,
        workspace.path(),
        "second loop",
        "artifacts/second.txt",
        "second\n",
    )?;

    for loop_id in [&first.loop_id, &second.loop_id] {
        runtime.handoff_to_caller_finalize(HandoffToCallerFinalizeRequest {
            loop_id: loop_id.to_string(),
        })?;
    }

    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: first.loop_id.clone(),
    })?;
    git(
        workspace.path(),
        &["cherry-pick", &first.accepted_commit_sha],
    )?;
    let first_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: first.loop_id,
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![first_head],
            resolution_notes: None,
        },
    })?;

    runtime.begin_caller_finalize(BeginCallerFinalizeRequest {
        loop_id: second.loop_id.clone(),
    })?;
    git(
        workspace.path(),
        &["cherry-pick", &second.accepted_commit_sha],
    )?;
    let second_head = git_output(workspace.path(), &["rev-parse", "HEAD"])?;
    let result = runtime.finalize_success(FinalizeSuccessRequest {
        loop_id: second.loop_id,
        integration_summary: CallerIntegrationSummary {
            strategy: "cherry_pick".to_owned(),
            landed_commit_shas: vec![second_head.clone()],
            resolution_notes: Some("Applied after caller branch advanced".to_owned()),
        },
    })?;
    assert_eq!(
        result["integration_summary"]["final_head_sha"],
        Value::String(second_head)
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn prepare_worktree_permission_denied_keeps_loop_open_for_mirrored_gitdir_retry() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "retry mirrored fallback".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "prepare-worktree permission failures should stay retryable for mirrored gitdir fallback"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let git_worktrees_dir = workspace.path().join(".git").join("worktrees");
    fs::create_dir_all(&git_worktrees_dir)?;
    let original_permissions = fs::metadata(&git_worktrees_dir)?.permissions();
    let mut readonly_permissions = original_permissions.clone();
    readonly_permissions.set_mode(0o555);
    fs::set_permissions(&git_worktrees_dir, readonly_permissions)?;

    let prepare_error = runtime
        .prepare_worktree(PrepareWorktreeRequest {
            loop_id: loop_response.loop_id.clone(),
        })
        .expect_err("expected primary gitdir permission failure to stay retryable");
    fs::set_permissions(&git_worktrees_dir, original_permissions)?;

    let error_text = format!("{prepare_error:#}");
    assert!(
        error_text.contains(".git/worktrees") && error_text.contains("Permission denied"),
        "unexpected error: {error_text}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (loop_status, loop_phase, worktree_lifecycle): (String, String, String) = conn.query_row(
        r#"
        SELECT loop.status, loop.phase, worktree.lifecycle
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__worktree_current worktree ON worktree.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1
        "#,
        params![loop_response.loop_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(loop_status, "open");
    assert_eq!(loop_phase, "awaiting_worktree");
    assert_eq!(worktree_lifecycle, "reserved");

    let failure_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND (event_name = 'SUBMIT_LOOP__worktree_prepare_failed' OR event_name = 'SUBMIT_LOOP__loop_failed')",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let result_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__result_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(failure_event_count, 0);
    assert_eq!(result_rows, 0);

    let worktree_path = materialize_loop_worktree_with_mirrored_gitdir(
        workspace.path(),
        &loop_response.branch,
        &loop_response.label,
    )?;
    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    assert_eq!(prepared["lifecycle"].as_str(), Some("prepared"));
    assert_eq!(
        prepared["path"].as_str(),
        Some(
            worktree_path
                .to_str()
                .context("non-utf8 mirrored worktree path")?
        )
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn prepare_worktree_reflog_permission_failure_keeps_loop_open_for_mirrored_gitdir_retry()
-> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "retry mirrored fallback from reflog permission failure".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "prepare-worktree reflog permission failures should stay retryable for mirrored gitdir fallback"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let git_logs_heads_dir = workspace
        .path()
        .join(".git")
        .join("logs")
        .join("refs")
        .join("heads");
    let original_permissions = fs::metadata(&git_logs_heads_dir)?.permissions();
    let mut readonly_permissions = original_permissions.clone();
    readonly_permissions.set_mode(0o555);
    fs::set_permissions(&git_logs_heads_dir, readonly_permissions)?;

    let prepare_error = runtime
        .prepare_worktree(PrepareWorktreeRequest {
            loop_id: loop_response.loop_id.clone(),
        })
        .expect_err("expected reflog permission failure to stay retryable");
    fs::set_permissions(&git_logs_heads_dir, original_permissions)?;

    let error_text = format!("{prepare_error:#}");
    assert!(
        error_text.contains(".git/logs/refs/heads") && error_text.contains("Permission denied"),
        "unexpected error: {error_text}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (loop_status, loop_phase, worktree_lifecycle): (String, String, String) = conn.query_row(
        r#"
        SELECT loop.status, loop.phase, worktree.lifecycle
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__worktree_current worktree ON worktree.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1
        "#,
        params![loop_response.loop_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(loop_status, "open");
    assert_eq!(loop_phase, "awaiting_worktree");
    assert_eq!(worktree_lifecycle, "reserved");

    let failure_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND (event_name = 'SUBMIT_LOOP__worktree_prepare_failed' OR event_name = 'SUBMIT_LOOP__loop_failed')",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(failure_event_count, 0);

    Ok(())
}

#[test]
fn prepare_worktree_reattaches_existing_loop_branch() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "reattach existing branch".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "prepare-worktree should reattach the authoritative loop branch when the branch exists but the reserved worktree is missing"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let branch_output = Command::new("git")
        .args(["branch", &loop_response.branch, "HEAD"])
        .current_dir(workspace.path())
        .output()
        .context("failed to create loop branch before prepare-worktree retry")?;
    if !branch_output.status.success() {
        bail!(
            "git branch {} HEAD failed\nstdout:\n{}\nstderr:\n{}",
            loop_response.branch,
            String::from_utf8_lossy(&branch_output.stdout),
            String::from_utf8_lossy(&branch_output.stderr)
        );
    }

    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worktree_path = PathBuf::from(
        prepared["path"]
            .as_str()
            .context("prepare-worktree response missing path")?,
    );

    assert_eq!(prepared["lifecycle"].as_str(), Some("prepared"));
    assert_eq!(
        prepared["branch"].as_str(),
        Some(loop_response.branch.as_str())
    );
    assert!(worktree_path.try_exists()?);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let failure_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND (event_name = 'SUBMIT_LOOP__worktree_prepare_failed' OR event_name = 'SUBMIT_LOOP__loop_failed')",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(failure_event_count, 0);

    Ok(())
}

#[test]
fn prepare_worktree_repairs_missing_prepared_worktree_path() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "repair missing prepared path".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "prepare-worktree should recreate the reserved worktree when prepared state exists but the path was removed out of band"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let first_prepare = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worktree_path = PathBuf::from(
        first_prepare["path"]
            .as_str()
            .context("prepare-worktree response missing path")?,
    );
    let remove_output = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to remove prepared worktree out of band")?;
    if !remove_output.status.success() {
        bail!(
            "git worktree remove --force {} failed\nstdout:\n{}\nstderr:\n{}",
            worktree_path.display(),
            String::from_utf8_lossy(&remove_output.stdout),
            String::from_utf8_lossy(&remove_output.stderr)
        );
    }
    assert!(!worktree_path.try_exists()?);

    let repaired_prepare = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

    assert_eq!(repaired_prepare["lifecycle"].as_str(), Some("prepared"));
    assert_eq!(
        repaired_prepare["path"].as_str(),
        Some(
            worktree_path
                .to_str()
                .context("non-utf8 repaired worktree path")?
        )
    );
    assert!(worktree_path.try_exists()?);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let prepared_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__worktree_prepared'",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(prepared_event_count, 2);

    Ok(())
}

#[test]
fn prepare_worktree_repairs_missing_registered_worktree_path() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "repair missing registered path".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "prepare-worktree should recover when the reserved worktree path was deleted out of band but git still has the worktree registered"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let first_prepare = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worktree_path = PathBuf::from(
        first_prepare["path"]
            .as_str()
            .context("prepare-worktree response missing path")?,
    );
    fs::remove_dir_all(&worktree_path)
        .with_context(|| format!("failed to remove {} out of band", worktree_path.display()))?;
    assert!(!worktree_path.try_exists()?);

    let registration_before = git_output(workspace.path(), &["worktree", "list", "--porcelain"])?;
    let worktree_line = format!("worktree {}", worktree_path.display());
    assert!(
        registration_before.contains(&worktree_line),
        "expected git to keep the removed worktree registered before repair:\n{registration_before}"
    );

    let repaired_prepare = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

    assert_eq!(repaired_prepare["lifecycle"].as_str(), Some("prepared"));
    assert_eq!(
        repaired_prepare["path"].as_str(),
        Some(
            worktree_path
                .to_str()
                .context("non-utf8 repaired worktree path")?
        )
    );
    assert!(worktree_path.try_exists()?);

    let registration_after = git_output(workspace.path(), &["worktree", "list", "--porcelain"])?;
    assert_eq!(
        registration_after.matches(&worktree_line).count(),
        1,
        "expected exactly one repaired worktree registration after recovery:\n{registration_after}"
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let failure_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND (event_name = 'SUBMIT_LOOP__worktree_prepare_failed' OR event_name = 'SUBMIT_LOOP__loop_failed')",
        params![loop_response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(failure_event_count, 0);

    Ok(())
}

#[test]
fn prepare_worktree_ignores_stale_mirrored_gitdir_when_repairing_primary_registration() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "repair missing registered path with stale mirror".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "prepare-worktree should fall back to the primary gitdir when a stale git-common directory exists"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let first_prepare = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;
    let worktree_path = PathBuf::from(
        first_prepare["path"]
            .as_str()
            .context("prepare-worktree response missing path")?,
    );
    fs::remove_dir_all(&worktree_path)?;
    assert!(!worktree_path.try_exists()?);

    let stale_mirror = workspace
        .path()
        .join(".loopy")
        .join(format!("git-common-{}", loop_response.label));
    fs::create_dir_all(&stale_mirror)?;
    let repaired_prepare = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

    assert_eq!(repaired_prepare["lifecycle"].as_str(), Some("prepared"));
    assert!(worktree_path.try_exists()?);

    let registration_after = git_output(workspace.path(), &["worktree", "list", "--porcelain"])?;
    assert!(
        registration_after.contains(&format!("worktree {}", worktree_path.display())),
        "expected repaired worktree to remain registered in the primary gitdir:\n{registration_after}"
    );

    Ok(())
}

#[test]
fn terminal_submission_auto_rebuilds_missing_loop_and_invocation_projections() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "auto rebuild".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "terminal submission should rebuild missing projections before validating".to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
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
    let invocation_context_path = invocation_context_path(workspace.path(), &worker.invocation_id);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    conn.execute(
        "DELETE FROM CORE__capability_current WHERE invocation_id = ?1",
        params![worker.invocation_id.clone()],
    )?;
    conn.execute(
        "DELETE FROM CORE__invocation_current WHERE invocation_id = ?1",
        params![worker.invocation_id.clone()],
    )?;
    conn.execute(
        "DELETE FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
    )?;

    let response = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path,
        submission_id: "rebuild-submit".to_owned(),
        checkpoints: vec![checkpoint("Recovered checkpoint")],
        improvement_opportunities: None,
        notes: Some("projection rebuild path".to_owned()),
    })?;

    assert_eq!(response.plan_revision, 1);
    let restored_loop_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let restored_invocation_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__invocation_current WHERE invocation_id = ?1",
        params![worker.invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(restored_loop_rows, 1);
    assert_eq!(restored_invocation_rows, 1);

    Ok(())
}

#[test]
fn finalize_failure_rebuilds_inconsistent_loop_projection_before_use() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "loop projection drift".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "coordinator APIs should rebuild drifted loop projections before use".to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let next_loop_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(loop_seq), 0) + 1 FROM CORE__events WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
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
        ) VALUES (?1, ?2, 'SUBMIT_LOOP__loop_failed', ?3, ?4, ?4)
        "#,
        params![
            loop_response.loop_id.clone(),
            next_loop_seq,
            json!({
                "failure_cause_type": "test_failure",
                "summary": "forced failure",
                "phase_at_failure": "awaiting_worktree",
                "last_stable_context": {
                    "base_commit_sha": "base",
                    "worktree_branch": loop_response.branch,
                    "worktree_label": loop_response.label,
                },
            })
            .to_string(),
            "2026-04-12T00:00:00Z",
        ],
    )?;
    conn.execute(
        "UPDATE SUBMIT_LOOP__loop_current SET worktree_branch = 'bogus-branch' WHERE loop_id = ?1",
        params![loop_response.loop_id.clone()],
    )?;

    let result = runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: loop_response.loop_id.clone(),
        failure_cause_type: "ignored_because_loop_already_failed".to_owned(),
        summary: "ignored".to_owned(),
    })?;
    assert_eq!(
        result["worktree_ref"]["branch"],
        json!(loop_response.branch)
    );

    Ok(())
}

#[test]
fn terminal_submission_rebuilds_inconsistent_capability_projection_before_validating() -> Result<()>
{
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "capability drift".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "terminal submissions should rebuild drifted capability rows before validating"
                .to_owned(),
        ),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
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
    let invocation_context_path = invocation_context_path(workspace.path(), &worker.invocation_id);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    conn.execute(
        r#"
        UPDATE CORE__capability_current
        SET token_state = 'consumed',
            accepted_api = 'SUBMIT_LOOP__submit_checkpoint_plan',
            accepted_submission_id = 'wrong-submission'
        WHERE invocation_id = ?1
        "#,
        params![worker.invocation_id.clone()],
    )?;

    let response = runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path,
        submission_id: "actual-submit".to_owned(),
        checkpoints: vec![checkpoint("Recovered checkpoint")],
        improvement_opportunities: None,
        notes: Some("projection rebuild path".to_owned()),
    })?;

    assert_eq!(response.plan_revision, 1);
    assert!(!response.idempotent);
    let (token_state, accepted_submission_id): (String, Option<String>) = conn.query_row(
        "SELECT token_state, accepted_submission_id FROM CORE__capability_current WHERE invocation_id = ?1",
        params![worker.invocation_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(token_state, "consumed");
    assert_eq!(accepted_submission_id.as_deref(), Some("actual-submit"));

    Ok(())
}

#[test]
fn failed_terminal_submission_does_not_persist_partial_events_or_token_state() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "terminal submission atomicity".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("accepted terminal submissions must stay invisible until commit".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
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
    let invocation_context_path = invocation_context_path(workspace.path(), &worker.invocation_id);

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_plan_submit
        BEFORE INSERT ON CORE__events
        WHEN NEW.event_name = 'SUBMIT_LOOP__plan_submitted'
        BEGIN
            SELECT RAISE(ABORT, 'forced plan submission failure');
        END;
        "#,
    )?;

    let error = runtime
        .submit_checkpoint_plan(SubmitCheckpointPlanRequest {
            invocation_context_path,
            submission_id: "plan-submit".to_owned(),
            checkpoints: vec![checkpoint("Checkpoint A")],
            improvement_opportunities: None,
            notes: None,
        })
        .expect_err("expected forced plan submission failure");
    assert!(
        error.to_string().contains("forced plan submission failure"),
        "unexpected error: {error:#}"
    );

    let terminal_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'CORE__terminal_api_called'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let plan_event_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__events WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'",
        params![loop_response.loop_id.clone()],
        |row| row.get(0),
    )?;
    let transcript_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM CORE__transcript_segments WHERE invocation_id = ?1",
        params![worker.invocation_id.clone()],
        |row| row.get(0),
    )?;
    let token_state: String = conn.query_row(
        "SELECT token_state FROM CORE__capability_current WHERE invocation_id = ?1",
        params![worker.invocation_id],
        |row| row.get(0),
    )?;
    assert_eq!(terminal_event_count, 0);
    assert_eq!(plan_event_count, 0);
    assert_eq!(transcript_count, worker.transcript_segment_count as i64);
    assert_eq!(token_state, "available");

    Ok(())
}

#[test]
fn open_connection_skips_review_updated_at_backfill_once_review_rows_are_populated() -> Result<()> {
    let workspace = git_workspace()?;
    install_bundle_into_workspace(workspace.path())?;
    let runtime = Runtime::new(workspace.path())?;
    let reviewed_loop = runtime.open_loop(OpenLoopRequest {
        summary: "backfill migration".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("bootstrap should not rescan populated review rows forever".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: reviewed_loop.loop_id.clone(),
    })?;
    let worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: reviewed_loop.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace.path(), &worker.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![checkpoint("Checkpoint A")],
        improvement_opportunities: None,
        notes: Some("populate review current".to_owned()),
    })?;
    runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: reviewed_loop.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let populated_review_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__review_current WHERE updated_at <> ''",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(populated_review_rows, 1);

    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_review_updated_at_backfill
        BEFORE UPDATE OF updated_at ON SUBMIT_LOOP__review_current
        BEGIN
            SELECT RAISE(ABORT, 'unexpected review updated_at backfill');
        END;
        "#,
    )?;

    let next_loop = runtime.open_loop(OpenLoopRequest {
        summary: "second open".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("opening another loop should not rewrite existing review rows".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;
    assert!(!next_loop.loop_id.is_empty());

    Ok(())
}

fn invocation_context_path(workspace_root: &Path, invocation_id: &str) -> PathBuf {
    workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"))
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
