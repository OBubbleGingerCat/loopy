use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::{
    EnsureNodeIdRequest, EnsureNodeIdResponse, EnsurePlanRequest, EnsurePlanResponse,
    OpenPlanRequest, OpenPlanResponse,
};

const ACTIVE_PLAN_STATUS: &str = "active";

pub(crate) fn ensure_plan(
    connection: &Connection,
    workspace_root: &Path,
    plan_root: &Path,
    request: EnsurePlanRequest,
) -> Result<EnsurePlanResponse> {
    let EnsurePlanRequest {
        plan_name,
        task_type,
        project_directory: _project_directory,
    } = request;

    if let Some(existing) = select_plan(connection, workspace_root, &plan_name)? {
        return Ok(EnsurePlanResponse {
            plan_id: existing.plan_id,
            plan_root: existing.plan_root,
            plan_status: existing.plan_status,
        });
    }

    let plan_id = Uuid::new_v4().to_string();
    let timestamp = current_timestamp()?;
    let workspace_root = workspace_root_string(workspace_root);
    let plan_root = path_string(plan_root);
    connection
        .execute(
            "INSERT INTO GEN_PLAN__plans (
                plan_id,
                workspace_root,
                plan_name,
                plan_root,
                task_type,
                plan_status,
                created_at,
                updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                plan_id,
                workspace_root,
                plan_name,
                plan_root,
                task_type,
                ACTIVE_PLAN_STATUS,
                timestamp,
                timestamp,
            ],
        )
        .context("failed to persist plan metadata")?;

    Ok(EnsurePlanResponse {
        plan_id,
        plan_root,
        plan_status: ACTIVE_PLAN_STATUS.to_owned(),
    })
}

pub(crate) fn open_plan(
    connection: &Connection,
    workspace_root: &Path,
    request: OpenPlanRequest,
) -> Result<OpenPlanResponse> {
    let plan_name = request.plan_name;
    let plan = select_plan(connection, workspace_root, &plan_name)?
        .ok_or_else(|| anyhow!("plan `{plan_name}` does not exist"))?;

    Ok(OpenPlanResponse {
        plan_id: plan.plan_id,
        plan_root: plan.plan_root,
        plan_status: plan.plan_status,
        task_type: plan.task_type,
    })
}

pub(crate) fn ensure_node_id(
    connection: &Connection,
    request: EnsureNodeIdRequest,
) -> Result<EnsureNodeIdResponse> {
    let EnsureNodeIdRequest {
        plan_id,
        relative_path,
        parent_relative_path,
    } = request;

    let node_id = ensure_node_id_for_path(
        connection,
        &plan_id,
        &relative_path,
        parent_relative_path.as_deref(),
    )?;
    Ok(EnsureNodeIdResponse { node_id })
}

fn ensure_node_id_for_path(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
    parent_relative_path: Option<&str>,
) -> Result<String> {
    if let Some(node_id) = select_node_id(connection, plan_id, relative_path)? {
        return Ok(node_id);
    }

    let parent_node_id = match parent_relative_path {
        Some(parent_relative_path) if parent_relative_path == relative_path => {
            return Err(anyhow!(
                "parent_relative_path must differ from relative_path for `{relative_path}`"
            ));
        }
        Some(parent_relative_path) => Some(ensure_node_id_for_path(
            connection,
            plan_id,
            parent_relative_path,
            None,
        )?),
        None => None,
    };

    let node_id = Uuid::new_v4().to_string();
    let node_name = node_name(relative_path);
    let timestamp = current_timestamp()?;

    if let Err(error) = connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id,
            node_id,
            relative_path,
            node_name,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            plan_id,
            node_id,
            relative_path,
            node_name,
            parent_node_id,
            timestamp,
            timestamp,
        ],
    ) {
        if let Some(existing) = select_node_id(connection, plan_id, relative_path)? {
            return Ok(existing);
        }
        return Err(error).context("failed to persist node metadata");
    }

    Ok(node_id)
}

fn select_plan(
    connection: &Connection,
    workspace_root: &Path,
    plan_name: &str,
) -> Result<Option<PlanRow>> {
    connection
        .query_row(
            "SELECT plan_id, plan_root, plan_status, task_type
             FROM GEN_PLAN__plans
             WHERE workspace_root = ?1 AND plan_name = ?2",
            params![workspace_root_string(workspace_root), plan_name],
            |row| {
                Ok(PlanRow {
                    plan_id: row.get(0)?,
                    plan_root: row.get(1)?,
                    plan_status: row.get(2)?,
                    task_type: row.get(3)?,
                })
            },
        )
        .optional()
        .context("failed to read persisted plan metadata")
}

fn select_node_id(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND relative_path = ?2",
            params![plan_id, relative_path],
            |row| row.get(0),
        )
        .optional()
        .context("failed to read persisted node metadata")
}

fn current_timestamp() -> Result<String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the unix epoch")?
        .as_secs()
        .to_string())
}

fn workspace_root_string(workspace_root: &Path) -> String {
    path_string(workspace_root)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn node_name(relative_path: &str) -> String {
    PathBuf::from(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(relative_path)
        .to_owned()
}

struct PlanRow {
    plan_id: String,
    plan_root: String,
    plan_status: String,
    task_type: String,
}
