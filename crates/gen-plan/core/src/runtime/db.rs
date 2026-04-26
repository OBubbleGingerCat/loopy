use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, Error as SqliteError};

pub const FIXED_DB_RELATIVE_PATH: &str = ".loopy/loopy.db";
pub(crate) const PROJECT_DIRECTORY_SOURCE_EXPLICIT: &str = "explicit";
pub(crate) const PROJECT_DIRECTORY_SOURCE_BACKFILLED_LEGACY: &str = "backfilled_legacy";

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
                project_directory_source TEXT NOT NULL DEFAULT 'explicit',
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
                node_kind TEXT NOT NULL DEFAULT '',
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
    let project_directory_added = ensure_column_exists(
        connection,
        "GEN_PLAN__plans",
        "project_directory",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column_exists(
        connection,
        "GEN_PLAN__plans",
        "project_directory_source",
        "TEXT NOT NULL DEFAULT 'explicit'",
    )?;
    ensure_column_exists(
        connection,
        "GEN_PLAN__nodes",
        "node_kind",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    backfill_node_kinds(connection)?;
    if project_directory_added {
        connection
            .execute(
                "UPDATE GEN_PLAN__plans
                 SET project_directory = workspace_root,
                     project_directory_source = ?1
                 WHERE project_directory = ''",
                [PROJECT_DIRECTORY_SOURCE_BACKFILLED_LEGACY],
            )
            .context("failed to backfill project_directory for migrated plans")?;
    }
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
    ensure_column_exists(
        connection,
        "GEN_PLAN__frontier_gate_runs",
        "invalidated_leaf_node_ids_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    Ok(())
}

fn backfill_node_kinds(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare(
            "SELECT nodes.plan_id, nodes.node_id, nodes.relative_path, plans.plan_name
             FROM GEN_PLAN__nodes nodes
             JOIN GEN_PLAN__plans plans ON plans.plan_id = nodes.plan_id
             WHERE nodes.node_kind = ''
                OR nodes.node_kind NOT IN ('parent', 'leaf')
                OR (
                    nodes.node_kind = 'leaf'
                    AND nodes.relative_path = plans.plan_name || '.md'
                )",
        )
        .context("failed to prepare node_kind backfill query")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .context("failed to query node_kind backfill rows")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read node_kind backfill rows")?;

    for (plan_id, node_id, relative_path, plan_name) in rows {
        let Some(node_kind) = infer_node_kind(&relative_path, &plan_name) else {
            continue;
        };
        connection
            .execute(
                "UPDATE GEN_PLAN__nodes
                 SET node_kind = ?1
                 WHERE plan_id = ?2 AND node_id = ?3",
                [node_kind, &plan_id, &node_id],
            )
            .with_context(|| {
                format!("failed to backfill node_kind for `{relative_path}` in plan `{plan_id}`")
            })?;
    }

    Ok(())
}

fn ensure_column_exists(
    connection: &Connection,
    table_name: &str,
    column_name: &str,
    column_definition: &str,
) -> Result<bool> {
    let alter_sql =
        format!("ALTER TABLE {table_name} ADD COLUMN {column_name} {column_definition}");
    match connection.execute(&alter_sql, []) {
        Ok(_) => Ok(true),
        Err(error) if is_duplicate_column_error(&error) => Ok(false),
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

fn infer_node_kind(relative_path: &str, plan_name: &str) -> Option<&'static str> {
    let path = PathBuf::from(relative_path);
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".md")?;
    if stem.is_empty() {
        return None;
    }
    if path.components().count() == 1 && stem == plan_name {
        return Some("parent");
    }
    let parent_dir_name = path.parent()?.file_name().and_then(|name| name.to_str());
    if parent_dir_name == Some(stem) {
        Some("parent")
    } else {
        Some("leaf")
    }
}
