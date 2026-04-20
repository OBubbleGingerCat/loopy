use anyhow::{Context, Result};
use rusqlite::Connection;

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
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .with_context(|| format!("failed to inspect schema for {table_name}"))?;
    let has_column = statement
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("failed to enumerate columns for {table_name}"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read columns for {table_name}"))?
        .into_iter()
        .any(|name| name == column_name);

    if !has_column {
        connection
            .execute(
                &format!("ALTER TABLE {table_name} ADD COLUMN {column_name} {column_definition}"),
                [],
            )
            .with_context(|| format!("failed to add {column_name} column to {table_name}"))?;
    }

    Ok(())
}
