use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use loopy::{
    CallerIntegrationSummary, FinalizeFailureRequest, FinalizeSuccessRequest, OpenLoopRequest,
    PrepareWorktreeRequest, Runtime,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn finalize_failure_materializes_caller_visible_failure_payload() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = Runtime::with_installed_skill_root(
        workspace.path(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "materialize failure result".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("coordinator should produce a final failure payload".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let failure_result = runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: loop_response.loop_id.clone(),
        failure_cause_type: "coordinator_failure".to_owned(),
        summary: "coordination policy exhausted".to_owned(),
    })?;

    assert_eq!(
        failure_result["loop_id"],
        Value::String(loop_response.loop_id.clone())
    );
    assert_eq!(
        failure_result["status"],
        Value::String("failure".to_owned())
    );
    assert_eq!(
        failure_result["failure_cause_type"],
        Value::String("coordinator_failure".to_owned())
    );
    assert_eq!(
        failure_result["summary"],
        Value::String("coordination policy exhausted".to_owned())
    );
    assert!(failure_result["source_event_id"].as_i64().is_some());
    assert!(failure_result["phase_at_failure"].as_str().is_some());
    assert!(failure_result["last_stable_context"].is_object());
    assert_eq!(
        failure_result["worktree_ref"]["label"],
        Value::String(loop_response.label)
    );
    assert!(failure_result["result_generated_at"].as_str().is_some());

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (status, result_ref, generated_at): (String, String, String) = conn.query_row(
        "SELECT status, result_ref, generated_at FROM CORE__result_current WHERE loop_id = ?1",
        params![loop_response.loop_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(status, "failure");
    assert_eq!(
        Some(generated_at.as_str()),
        failure_result["result_generated_at"].as_str()
    );
    let stored_payload_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        params![result_ref],
        |row| row.get(0),
    )?;
    let stored_payload: Value = serde_json::from_str(&stored_payload_json)?;
    assert_eq!(stored_payload, failure_result);

    Ok(())
}

#[test]
fn finalize_failure_reuses_failure_event_id_when_materializing_recovered_results() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = Runtime::with_installed_skill_root(
        workspace.path(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "recover failure event id".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some(
            "recovered failure results should preserve the original loop_failed event id"
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
                "failure_cause_type": "legacy_failure",
                "summary": "legacy failure payload",
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
    let failure_event_id: i64 = conn.query_row(
        "SELECT event_id FROM CORE__events WHERE loop_id = ?1 AND loop_seq = ?2",
        params![loop_response.loop_id.clone(), next_loop_seq],
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
        ) VALUES (?1, ?2, 'SUBMIT_LOOP__worktree_cleanup_warning', ?3, ?4, ?4)
        "#,
        params![
            loop_response.loop_id.clone(),
            next_loop_seq + 1,
            json!({
                "summary": "legacy cleanup warning",
                "worktree_path": workspace.path().join(".loopy/worktrees").join(&loop_response.label),
                "worktree_branch": loop_response.branch,
                "worktree_label": loop_response.label,
            })
            .to_string(),
            "2026-04-12T00:00:01Z",
        ],
    )?;
    runtime.rebuild_loop_projections(&loop_response.loop_id)?;

    let failure_result = runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: loop_response.loop_id.clone(),
        failure_cause_type: "ignored".to_owned(),
        summary: "ignored".to_owned(),
    })?;

    assert_eq!(
        failure_result["status"],
        Value::String("failure".to_owned())
    );
    assert_eq!(
        failure_result["source_event_id"],
        Value::from(failure_event_id)
    );

    Ok(())
}

#[test]
fn finalize_success_rejects_loops_with_materialized_failure_results() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = Runtime::with_installed_skill_root(
        workspace.path(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "reject finalize after failure".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("success finalization must not reuse failure payloads".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let failure_result = runtime.finalize_failure(FinalizeFailureRequest {
        loop_id: loop_response.loop_id.clone(),
        failure_cause_type: "coordinator_failure".to_owned(),
        summary: "coordination policy exhausted".to_owned(),
    })?;
    assert_eq!(
        failure_result["status"],
        Value::String("failure".to_owned())
    );

    let error = runtime
        .finalize_success(FinalizeSuccessRequest {
            loop_id: loop_response.loop_id,
            integration_summary: CallerIntegrationSummary {
                strategy: "cherry_pick".to_owned(),
                landed_commit_shas: vec!["deadbeef".to_owned()],
                resolution_notes: None,
            },
        })
        .expect_err("expected finalize_success to reject loops with failure results");
    assert!(
        error.to_string().contains("failure result"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn bundled_cli_can_finalize_failure_in_one_command() -> Result<()> {
    let install_root = install_bundle()?;
    let workspace = git_workspace()?;
    let bundled_loopy = install_root.join("bin/loopy");

    let open_loop_output = Command::new(&bundled_loopy)
        .args([
            "open-loop",
            "--summary",
            "cli failure result",
            "--task-type",
            "coding-task",
            "--context",
            "exercise finalize-failure command",
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

    let finalize_output = Command::new(&bundled_loopy)
        .args([
            "finalize-failure",
            "--loop-id",
            &loop_id,
            "--failure-cause-type",
            "coordinator_failure",
            "--summary",
            "bundled cli failure",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run bundled finalize-failure command")?;
    if !finalize_output.status.success() {
        bail!(
            "finalize-failure failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&finalize_output.stdout),
            String::from_utf8_lossy(&finalize_output.stderr)
        );
    }
    let failure_json: Value = serde_json::from_slice(&finalize_output.stdout)?;
    assert_eq!(failure_json["status"], Value::String("failure".to_owned()));
    assert_eq!(
        failure_json["failure_cause_type"],
        Value::String("coordinator_failure".to_owned())
    );

    Ok(())
}

#[test]
fn prepare_worktree_failure_materializes_a_failure_result() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = Runtime::with_installed_skill_root(
        workspace.path(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?;
    let loop_response = runtime.open_loop(OpenLoopRequest {
        summary: "worktree failure result".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("prepare-worktree failure should materialize a failure result".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let conflicting_path = workspace
        .path()
        .join(".loopy")
        .join("worktrees")
        .join(&loop_response.label);
    fs::create_dir_all(&conflicting_path)?;
    fs::write(
        conflicting_path.join("not-a-worktree.txt"),
        "occupy reserved worktree path\n",
    )?;
    let failure_result = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: loop_response.loop_id.clone(),
    })?;

    assert_eq!(
        failure_result["status"],
        Value::String("failure".to_owned())
    );
    assert_eq!(
        failure_result["failure_cause_type"],
        Value::String("worktree_prepare_failed".to_owned())
    );
    assert!(failure_result["summary"].as_str().is_some());
    assert_eq!(
        failure_result["loop_id"],
        Value::String(loop_response.loop_id.clone())
    );

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (loop_status, worktree_lifecycle): (String, String) = conn.query_row(
        r#"
        SELECT loop.status, worktree.lifecycle
        FROM SUBMIT_LOOP__loop_current loop
        JOIN SUBMIT_LOOP__worktree_current worktree ON worktree.loop_id = loop.loop_id
        WHERE loop.loop_id = ?1
        "#,
        params![loop_response.loop_id.clone()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(loop_status, "failed");
    assert_eq!(worktree_lifecycle, "prepare_failed");

    let event_names: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT event_name FROM CORE__events WHERE loop_id = ?1 AND (event_name LIKE 'SUBMIT_LOOP__worktree_%' OR event_name = 'SUBMIT_LOOP__loop_failed') ORDER BY event_id ASC",
        )?;
        let rows = statement.query_map(params![loop_response.loop_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(
        event_names,
        vec![
            "SUBMIT_LOOP__worktree_prepare_failed".to_owned(),
            "SUBMIT_LOOP__loop_failed".to_owned(),
        ]
    );

    Ok(())
}

fn install_bundle() -> Result<PathBuf> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
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
