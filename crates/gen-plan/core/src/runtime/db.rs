use anyhow::{Context, Result};
use rusqlite::{Connection, Error as SqliteError};

pub const FIXED_DB_RELATIVE_PATH: &str = ".loopy/loopy.db";

pub(crate) fn bootstrap_schema(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS GEN_PLAN__plans (
                plan_id TEXT PRIMARY KEY,
                workspace_root TEXT NOT NULL,
                plan_name TEXT NOT NULL,
                plan_root TEXT NOT NULL,
                project_directory TEXT NOT NULL,
                task_type TEXT NOT NULL,
                plan_status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(workspace_root, plan_name)
            );

            CREATE TABLE IF NOT EXISTS GEN_PLAN__nodes (
                plan_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                node_name TEXT NOT NULL,
                parent_node_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(plan_id, node_id),
                UNIQUE(plan_id, relative_path),
                FOREIGN KEY(plan_id) REFERENCES GEN_PLAN__plans(plan_id) ON DELETE CASCADE,
                FOREIGN KEY(plan_id, parent_node_id) REFERENCES GEN_PLAN__nodes(plan_id, node_id)
                    ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS GEN_PLAN__leaf_gate_runs (
                leaf_gate_run_id TEXT PRIMARY KEY,
                plan_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                planner_mode TEXT NOT NULL,
                reviewer_role_id TEXT NOT NULL,
                passed INTEGER NOT NULL,
                verdict TEXT NOT NULL,
                summary TEXT NOT NULL DEFAULT '',
                issues_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY(plan_id) REFERENCES GEN_PLAN__plans(plan_id) ON DELETE CASCADE,
                FOREIGN KEY(plan_id, node_id) REFERENCES GEN_PLAN__nodes(plan_id, node_id)
                    ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS GEN_PLAN__frontier_gate_runs (
                frontier_gate_run_id TEXT PRIMARY KEY,
                plan_id TEXT NOT NULL,
                parent_node_id TEXT NOT NULL,
                planner_mode TEXT NOT NULL,
                reviewer_role_id TEXT NOT NULL,
                passed INTEGER NOT NULL,
                verdict TEXT NOT NULL,
                summary TEXT NOT NULL DEFAULT '',
                issues_json TEXT NOT NULL,
                invalidated_leaf_node_ids_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY(plan_id) REFERENCES GEN_PLAN__plans(plan_id) ON DELETE CASCADE,
                FOREIGN KEY(plan_id, parent_node_id) REFERENCES GEN_PLAN__nodes(plan_id, node_id)
                    ON DELETE CASCADE
            );
            "#,
        )
        .context("failed to bootstrap gen-plan runtime schema")?;
    ensure_column_exists(
        connection,
        "GEN_PLAN__plans",
        "project_directory",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    connection
        .execute(
            "UPDATE GEN_PLAN__plans
             SET project_directory = workspace_root
             WHERE project_directory = ''",
            [],
        )
        .context("failed to backfill project_directory for migrated plans")?;
    ensure_column_exists(
        connection,
        "GEN_PLAN__leaf_gate_runs",
        "summary",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column_exists(
        connection,
        "GEN_PLAN__frontier_gate_runs",
        "summary",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    Ok(())
}

fn ensure_column_exists(
    connection: &Connection,
    table_name: &str,
    column_name: &str,
    column_definition: &str,
) -> Result<()> {
    let alter_sql =
        format!("ALTER TABLE {table_name} ADD COLUMN {column_name} {column_definition}");
    match connection.execute(&alter_sql, []) {
        Ok(_) => Ok(()),
        Err(error) if is_duplicate_column_error(&error) => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to add {column_name} column to {table_name}")),
    }
}

fn is_duplicate_column_error(error: &SqliteError) -> bool {
    match error {
        SqliteError::SqliteFailure(_, Some(message)) => message
            .to_ascii_lowercase()
            .contains("duplicate column name"),
        _ => false,
    }
}
