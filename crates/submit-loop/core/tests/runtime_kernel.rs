mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use loopy::{OpenLoopRequest, Runtime, ShowLoopRequest};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;
use toml::Value as TomlValue;

#[test]
fn open_loop_bootstraps_fixed_database_and_records_loop_opened_event() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = runtime_with_repo_skill(workspace.path())?;

    let response = runtime.open_loop(OpenLoopRequest {
        summary: "bootstrap runtime".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("need initial durable loop state".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let db_path = workspace.path().join(".loopy/loopy.db");
    assert_eq!(response.db_path, db_path);
    assert!(
        db_path.exists(),
        "expected database at {}",
        db_path.display()
    );

    let conn = Connection::open(&db_path)?;
    assert!(table_exists(&conn, "CORE__events")?);
    assert!(table_exists(&conn, "CORE__contents")?);
    assert!(table_exists(&conn, "CORE__invocation_current")?);
    assert!(table_exists(&conn, "CORE__capability_current")?);
    assert!(table_exists(&conn, "CORE__result_current")?);
    assert!(table_exists(&conn, "SUBMIT_LOOP__loop_current")?);
    assert!(table_exists(&conn, "SUBMIT_LOOP__plan_current")?);
    assert!(table_exists(&conn, "SUBMIT_LOOP__checkpoint_current")?);
    assert!(table_exists(&conn, "SUBMIT_LOOP__review_current")?);
    assert!(table_exists(&conn, "SUBMIT_LOOP__worktree_current")?);
    assert!(table_exists(&conn, "SUBMIT_LOOP__commit_current")?);

    let mut statement = conn.prepare(
        "SELECT event_name FROM CORE__events WHERE loop_id = ? ORDER BY loop_seq ASC LIMIT 1",
    )?;
    let first_event_name: String = statement.query_row([response.loop_id], |row| row.get(0))?;
    assert_eq!(first_event_name, "SUBMIT_LOOP__loop_opened");
    git_branch_is_creatable(workspace.path(), &response.branch)?;

    Ok(())
}

#[test]
fn open_loop_normalizes_optional_inputs_and_persists_resolved_role_selection() -> Result<()> {
    let workspace = git_workspace()?;
    let skill_root = crate::support::submit_loop_source_root().as_path();
    let runtime = Runtime::with_installed_skill_root(workspace.path(), skill_root)?;

    let response = runtime.open_loop(OpenLoopRequest {
        summary: "normalize caller input".to_owned(),
        task_type: "coding-task".to_owned(),
        context: None,
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: None,
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (loop_input_ref, resolved_role_selection_ref): (String, String) = conn.query_row(
        "SELECT loop_input_ref, resolved_role_selection_ref FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        [response.loop_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let loop_input = read_content(&conn, &loop_input_ref)?;
    assert_eq!(loop_input["summary"], json!("normalize caller input"));
    assert_eq!(loop_input["task_type"], json!("coding-task"));
    assert_eq!(loop_input["context"], json!(""));
    assert_eq!(loop_input["constraints"], json!({}));
    assert_eq!(loop_input["bypass_sandbox"], json!(false));

    let role_selection = read_content(&conn, &resolved_role_selection_ref)?;
    assert_eq!(role_selection["task_type"], json!("coding-task"));
    assert_eq!(role_selection["planning_worker"], json!("codex_planner"));
    assert_eq!(
        role_selection["artifact_worker"],
        json!("codex_implementer")
    );
    assert_eq!(
        role_selection["checkpoint_reviewers"],
        json!(["codex_scope", "codex_plan", "codex_contract"])
    );
    assert_eq!(role_selection["artifact_reviewers"], json!(["mock"]));

    Ok(())
}

#[test]
fn runtime_uses_dev_registry_to_resolve_repo_local_skill_root() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let runtime = Runtime::new(repo_root)?;

    assert_eq!(
        runtime.installed_skill_root()?,
        crate::support::submit_loop_source_root().clone()
    );

    Ok(())
}

#[test]
fn open_loop_rejects_legacy_request_shape_and_explicit_default_reviewer_ids() -> Result<()> {
    let error = serde_json::from_value::<OpenLoopRequest>(json!({
        "summary": "legacy request shape",
        "task_type": "coding-task",
        "context": "legacy callers should still map worker/default reviewer ids",
        "worker": "mock_worker",
        "checkpoint_reviewers": ["default"],
        "artifact_reviewers": ["default"],
        "constraints": {},
        "bypass_sandbox": false,
        "coordinator_prompt": "You are the coordinator.",
    }))
    .expect_err("legacy open-loop request shape should be rejected");
    let error = error.to_string();
    assert_eq!(
        error.contains("worker"),
        true,
        "unexpected legacy request error: {error}"
    );

    Ok(())
}

#[test]
fn open_loop_rejects_legacy_default_worker_in_task_type_config() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    fs::write(
        install_root.join("roles/coding-task/task-type.toml"),
        [
            "task_type = \"coding-task\"",
            "default_worker = \"codex_worker\"",
            "default_checkpoint_reviewers = [\"mock\"]",
            "default_artifact_reviewers = [\"mock\"]",
            "",
        ]
        .join("\n"),
    )?;
    let runtime = Runtime::new(workspace.path())?;

    let error = runtime
        .open_loop(OpenLoopRequest {
            summary: "legacy installed defaults".to_owned(),
            task_type: "coding-task".to_owned(),
            context: Some("older installed bundles should stop opening loops".to_owned()),
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: Some(json!({})),
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("legacy default_worker config should be rejected");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("default_worker"),
        "unexpected default_worker error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_loop_rejects_task_type_config_that_omits_task_type() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    fs::write(
        install_root.join("roles/coding-task/task-type.toml"),
        [
            "default_planning_worker = \"codex_planner\"",
            "default_artifact_worker = \"codex_implementer\"",
            "default_checkpoint_reviewers = [\"mock\"]",
            "default_artifact_reviewers = [\"mock\"]",
            "",
        ]
        .join("\n"),
    )?;
    let runtime = Runtime::new(workspace.path())?;

    let error = runtime
        .open_loop(OpenLoopRequest {
            summary: "omitted task_type defaults".to_owned(),
            task_type: "coding-task".to_owned(),
            context: Some(
                "installed bundles must declare task_type explicitly after the hard cut".to_owned(),
            ),
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: Some(json!({})),
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("task-type configs without task_type should be rejected");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("task_type"),
        "unexpected omitted task_type error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_loop_rejects_legacy_default_reviewers_from_installed_bundle() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    fs::write(
        install_root.join("roles/coding-task/task-type.toml"),
        [
            "task_type = \"coding-task\"",
            "default_planning_worker = \"codex_planner\"",
            "default_artifact_worker = \"codex_implementer\"",
            "default_checkpoint_reviewers = [\"default\"]",
            "default_artifact_reviewers = [\"default\"]",
            "",
        ]
        .join("\n"),
    )?;
    let runtime = Runtime::new(workspace.path())?;

    let error = runtime
        .open_loop(OpenLoopRequest {
            summary: "legacy default reviewers".to_owned(),
            task_type: "coding-task".to_owned(),
            context: Some("default reviewer aliases should no longer expand".to_owned()),
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: Some(json!({})),
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("legacy default reviewer ids should be rejected");
    assert!(
        error.to_string().contains("default"),
        "unexpected legacy default reviewer error: {error:#}"
    );

    Ok(())
}

#[test]
fn open_loop_rejects_mixed_legacy_default_reviewer_lists_from_installed_bundle() -> Result<()> {
    let workspace = git_workspace()?;
    let install_root = install_bundle_into_workspace(workspace.path())?;
    fs::write(
        install_root.join("roles/coding-task/task-type.toml"),
        [
            "task_type = \"coding-task\"",
            "default_planning_worker = \"codex_planner\"",
            "default_artifact_worker = \"codex_implementer\"",
            "default_checkpoint_reviewers = [\"default\", \"mock\"]",
            "default_artifact_reviewers = [\"default\", \"mock\"]",
            "",
        ]
        .join("\n"),
    )?;
    let runtime = Runtime::new(workspace.path())?;

    let error = runtime
        .open_loop(OpenLoopRequest {
            summary: "legacy mixed reviewer defaults".to_owned(),
            task_type: "coding-task".to_owned(),
            context: Some("mixed default reviewer aliases should no longer expand".to_owned()),
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: Some(json!({})),
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("mixed legacy default reviewer ids should be rejected");
    assert!(
        error.to_string().contains("default"),
        "unexpected mixed default reviewer error: {error:#}"
    );

    Ok(())
}

#[test]
fn coding_task_task_type_config_uses_split_worker_defaults() -> Result<()> {
    let skill_root = crate::support::submit_loop_source_root().as_path();
    let task_type_text = fs::read_to_string(skill_root.join("roles/coding-task/task-type.toml"))?;
    let task_type_config: TomlValue = toml::from_str(&task_type_text)?;

    assert_eq!(
        task_type_config.get("default_planning_worker"),
        Some(&TomlValue::String("codex_planner".to_owned()))
    );
    assert_eq!(
        task_type_config.get("default_artifact_worker"),
        Some(&TomlValue::String("codex_implementer".to_owned()))
    );
    assert_eq!(
        task_type_config.get("default_checkpoint_reviewers"),
        Some(&TomlValue::Array(vec![
            TomlValue::String("codex_scope".to_owned()),
            TomlValue::String("codex_plan".to_owned()),
            TomlValue::String("codex_contract".to_owned()),
        ]))
    );
    assert_eq!(
        task_type_config.get("default_artifact_reviewers"),
        Some(&TomlValue::Array(vec![
            TomlValue::String("codex_checkpoint_contract".to_owned()),
            TomlValue::String("codex_correctness".to_owned()),
            TomlValue::String("codex_code_quality".to_owned()),
        ]))
    );
    assert!(
        task_type_config.get("default_worker").is_none(),
        "coding-task config should remove the legacy default_worker key"
    );

    Ok(())
}

#[test]
fn open_loop_rejects_blank_fields_unknown_task_types_empty_reviewers_and_duplicate_reviewers()
-> Result<()> {
    let workspace = git_workspace()?;
    let skill_root = crate::support::submit_loop_source_root().as_path();
    let runtime = Runtime::with_installed_skill_root(workspace.path(), skill_root)?;

    let blank_summary = runtime
        .open_loop(OpenLoopRequest {
            summary: "   ".to_owned(),
            task_type: "coding-task".to_owned(),
            context: None,
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: None,
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("blank summary should be rejected");
    assert!(
        blank_summary.to_string().contains("summary"),
        "unexpected blank summary error: {blank_summary:#}"
    );

    let blank_task_type = runtime
        .open_loop(OpenLoopRequest {
            summary: "valid summary".to_owned(),
            task_type: "   ".to_owned(),
            context: None,
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: None,
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("blank task_type should be rejected");
    assert!(
        blank_task_type.to_string().contains("task_type"),
        "unexpected blank task_type error: {blank_task_type:#}"
    );

    let unknown_task_type = runtime
        .open_loop(OpenLoopRequest {
            summary: "valid summary".to_owned(),
            task_type: "unknown-task".to_owned(),
            context: None,
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: None,
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("unknown task_type should be rejected");
    assert!(
        unknown_task_type.to_string().contains("unknown-task"),
        "unexpected unknown task_type error: {unknown_task_type:#}"
    );

    let empty_checkpoint_reviewers = runtime
        .open_loop(OpenLoopRequest {
            summary: "valid summary".to_owned(),
            task_type: "coding-task".to_owned(),
            context: None,
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: Some(vec![]),
            artifact_reviewers: None,
            constraints: None,
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("empty checkpoint reviewers should be rejected");
    assert!(
        empty_checkpoint_reviewers
            .to_string()
            .contains("checkpoint_reviewers"),
        "unexpected empty checkpoint reviewers error: {empty_checkpoint_reviewers:#}"
    );

    let duplicate_artifact_reviewers = runtime
        .open_loop(OpenLoopRequest {
            summary: "valid summary".to_owned(),
            task_type: "coding-task".to_owned(),
            context: None,
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: Some(vec!["mock".to_owned(), "mock".to_owned()]),
            constraints: None,
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("duplicate artifact reviewers should be rejected");
    assert!(
        duplicate_artifact_reviewers
            .to_string()
            .contains("artifact_reviewers"),
        "unexpected duplicate artifact reviewers error: {duplicate_artifact_reviewers:#}"
    );

    Ok(())
}

#[test]
fn open_loop_rolls_back_allocated_content_and_projection_rows_when_commit_fails() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = runtime_with_repo_skill(workspace.path())?;
    fs::create_dir_all(workspace.path().join(".loopy"))?;
    runtime.rebuild_projections()?;
    let db_path = workspace.path().join(".loopy/loopy.db");
    let conn = Connection::open(&db_path)?;
    conn.execute_batch(
        r#"
        CREATE TRIGGER fail_loop_open
        BEFORE INSERT ON CORE__events
        WHEN NEW.event_name = 'SUBMIT_LOOP__loop_opened'
        BEGIN
            SELECT RAISE(ABORT, 'forced loop-open failure');
        END;
        "#,
    )?;

    let error = runtime
        .open_loop(OpenLoopRequest {
            summary: "bootstrap runtime".to_owned(),
            task_type: "coding-task".to_owned(),
            context: Some("failed loop open must not leak durable state".to_owned()),
            planning_worker: None,
            artifact_worker: None,
            checkpoint_reviewers: None,
            artifact_reviewers: None,
            constraints: Some(json!({})),
            bypass_sandbox: Some(false),
            coordinator_prompt: "You are the coordinator.".to_owned(),
        })
        .expect_err("expected loop open to fail");
    assert!(
        error.to_string().contains("forced loop-open failure"),
        "unexpected error: {error:#}"
    );

    let event_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__events", [], |row| row.get(0))?;
    let content_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM CORE__contents", [], |row| row.get(0))?;
    let loop_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__loop_current",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(event_rows, 0);
    assert_eq!(content_rows, 0);
    assert_eq!(loop_rows, 0);

    Ok(())
}

#[test]
fn rejects_database_path_overrides_outside_the_fixed_runtime_root() -> Result<()> {
    let workspace = git_workspace()?;
    let attempted_override = workspace.path().join("escape.db");
    let error = Runtime::with_db_path_override(workspace.path(), Some(attempted_override))
        .expect_err("runtime should reject overriding the durable store path");

    assert!(
        error.to_string().contains("./.loopy/loopy.db"),
        "expected fixed-root error, got: {error:#}"
    );

    Ok(())
}

#[test]
fn rebuild_projections_recovers_current_state_from_events() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = runtime_with_repo_skill(workspace.path())?;

    let response = runtime.open_loop(OpenLoopRequest {
        summary: "projection rebuild".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("ensure projections can be recovered".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let db_path = workspace.path().join(".loopy/loopy.db");
    let conn = Connection::open(&db_path)?;
    truncate_current_tables(&conn)?;

    runtime.rebuild_projections()?;

    let rebuilt_conn = Connection::open(&db_path)?;
    let loop_rows: i64 = rebuilt_conn.query_row(
        "SELECT COUNT(*) FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?",
        [response.loop_id],
        |row| row.get(0),
    )?;
    assert_eq!(loop_rows, 1, "expected loop_current row to be rebuilt");

    Ok(())
}

#[test]
fn rebuild_projections_rejects_legacy_plan_checkpoint_payloads() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = runtime_with_repo_skill(workspace.path())?;

    let response = runtime.open_loop(OpenLoopRequest {
        summary: "legacy plan replay".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("older plan events should remain replayable after upgrade".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let db_path = workspace.path().join(".loopy/loopy.db");
    let conn = Connection::open(&db_path)?;
    let next_loop_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(loop_seq), 0) + 1 FROM CORE__events WHERE loop_id = ?1",
        [&response.loop_id],
        |row| row.get(0),
    )?;
    let legacy_checkpoints = json!([
        {
            "checkpoint_id": "checkpoint-legacy",
            "sequence_index": 0,
            "title": "Legacy checkpoint",
            "revision": 1
        }
    ]);
    conn.execute(
        r#"
        INSERT INTO CORE__events (
            loop_id,
            loop_seq,
            event_name,
            payload_json,
            occurred_at,
            recorded_at
        ) VALUES (?1, ?2, 'SUBMIT_LOOP__plan_submitted', ?3, ?4, ?4)
        "#,
        params![
            response.loop_id.clone(),
            next_loop_seq,
            json!({
                "invocation_id": "legacy-invocation",
                "submission_id": "legacy-plan-submit",
                "plan_revision": 1,
                "checkpoints": legacy_checkpoints,
                "notes": "legacy plan payload",
            })
            .to_string(),
            "2026-04-12T00:00:00Z",
        ],
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
        ) VALUES (?1, ?2, 'SUBMIT_LOOP__plan_accepted', ?3, ?4, ?4)
        "#,
        params![
            response.loop_id.clone(),
            next_loop_seq + 1,
            json!({
                "review_round_id": "legacy-review-round",
                "plan_revision": 1,
                "checkpoints": legacy_checkpoints,
            })
            .to_string(),
            "2026-04-12T00:00:01Z",
        ],
    )?;

    truncate_current_tables(&conn)?;
    let error = runtime
        .rebuild_loop_projections(&response.loop_id)
        .expect_err("legacy plan payloads should be rejected during replay");
    assert_eq!(
        error.to_string().contains("acceptance")
            || error.to_string().contains("verification_steps")
            || error.to_string().contains("expected_outcomes"),
        true,
        "unexpected legacy plan replay error: {error:#}"
    );

    Ok(())
}

#[test]
fn show_loop_summary_is_stable_after_projection_rebuild() -> Result<()> {
    let workspace = git_workspace()?;
    let runtime = runtime_with_repo_skill(workspace.path())?;

    let response = runtime.open_loop(OpenLoopRequest {
        summary: "projection-backed show loop".to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("show_loop should survive explicit projection rebuild".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: None,
        artifact_reviewers: None,
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let before = runtime.show_loop(ShowLoopRequest {
        loop_id: response.loop_id.clone(),
    })?;

    let conn = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    truncate_current_tables(&conn)?;
    runtime.rebuild_projections()?;

    let after = runtime.show_loop(ShowLoopRequest {
        loop_id: response.loop_id,
    })?;

    assert_eq!(
        serde_json::to_value(&before)?,
        serde_json::to_value(&after)?
    );

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

fn table_exists(conn: &Connection, table_name: &str) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?)",
        [table_name],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists == 1)
}

fn read_content(conn: &Connection, content_ref: &str) -> Result<serde_json::Value> {
    let payload_json: String = conn.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        [content_ref],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&payload_json)?)
}

fn runtime_with_repo_skill(workspace_root: &Path) -> Result<Runtime> {
    Runtime::with_installed_skill_root(
        workspace_root,
        crate::support::submit_loop_source_root().as_path(),
    )
}

fn install_bundle_into_workspace(workspace_root: &Path) -> Result<std::path::PathBuf> {
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
    crate::support::write_submit_loop_dev_registry(workspace_root, &install_root)?;
    Ok(install_root)
}

fn truncate_current_tables(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM CORE__invocation_current", [])?;
    conn.execute("DELETE FROM CORE__capability_current", [])?;
    conn.execute("DELETE FROM CORE__result_current", [])?;
    conn.execute("DELETE FROM SUBMIT_LOOP__loop_current", [])?;
    conn.execute("DELETE FROM SUBMIT_LOOP__plan_current", [])?;
    conn.execute("DELETE FROM SUBMIT_LOOP__checkpoint_current", [])?;
    conn.execute("DELETE FROM SUBMIT_LOOP__review_current", [])?;
    conn.execute("DELETE FROM SUBMIT_LOOP__worktree_current", [])?;
    conn.execute("DELETE FROM SUBMIT_LOOP__commit_current", [])?;
    Ok(())
}

fn git_branch_is_creatable(repo_root: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", branch, "HEAD"])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to run git branch {branch} HEAD"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git branch failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
