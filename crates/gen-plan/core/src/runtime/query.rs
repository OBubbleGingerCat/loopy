use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::{
    EnsureNodeIdRequest, EnsureNodeIdResponse, EnsurePlanRequest, EnsurePlanResponse,
    OpenPlanRequest, OpenPlanResponse,
};

const ACTIVE_PLAN_STATUS: &str = "active";

#[derive(Debug, Clone)]
pub(crate) struct GatePlanContext {
    pub plan_id: String,
    pub task_type: String,
    pub plan_root: PathBuf,
    pub project_directory: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct NodeRecord {
    pub node_id: String,
    pub relative_path: String,
    pub parent_node_id: Option<String>,
}

pub(crate) fn ensure_plan(
    connection: &Connection,
    workspace_root: &Path,
    plan_root: &Path,
    request: EnsurePlanRequest,
) -> Result<EnsurePlanResponse> {
    let EnsurePlanRequest {
        plan_name,
        task_type,
        project_directory,
    } = request;
    let task_type = loopy_gen_plan_bundle::validate_task_type_identifier(&task_type)?;
    let project_directory = normalize_project_directory(workspace_root, &project_directory)?;

    if let Some(existing) = select_plan(connection, workspace_root, &plan_name, plan_root)? {
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
                project_directory,
                task_type,
                plan_status,
                created_at,
                updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                plan_id,
                workspace_root,
                plan_name,
                plan_root,
                project_directory,
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
    plan_root: &Path,
    request: OpenPlanRequest,
) -> Result<OpenPlanResponse> {
    let plan_name = request.plan_name;
    let plan = select_plan(connection, workspace_root, &plan_name, plan_root)?
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
    let relative_path = validate_plan_local_path("relative_path", &relative_path)?;
    let parent_relative_path = parent_relative_path
        .as_deref()
        .map(|path| validate_plan_local_path("parent_relative_path", path))
        .transpose()?;

    let node_id = ensure_node_id_for_path(
        connection,
        &plan_id,
        &relative_path,
        parent_relative_path.as_deref(),
    )?;
    Ok(EnsureNodeIdResponse { node_id })
}

pub(crate) fn load_gate_plan_context(
    connection: &Connection,
    plan_id: &str,
) -> Result<GatePlanContext> {
    connection
        .query_row(
            "SELECT plan_id, task_type, plan_root, project_directory
             FROM GEN_PLAN__plans
             WHERE plan_id = ?1",
            params![plan_id],
            |row| {
                Ok(GatePlanContext {
                    plan_id: row.get(0)?,
                    task_type: row.get(1)?,
                    plan_root: PathBuf::from(row.get::<_, String>(2)?),
                    project_directory: PathBuf::from(row.get::<_, String>(3)?),
                })
            },
        )
        .optional()
        .context("failed to load persisted plan context")?
        .ok_or_else(|| anyhow!("plan `{plan_id}` does not exist"))
}

pub(crate) fn load_node_record(
    connection: &Connection,
    plan_id: &str,
    node_id: &str,
) -> Result<NodeRecord> {
    connection
        .query_row(
            "SELECT node_id, relative_path, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND node_id = ?2",
            params![plan_id, node_id],
            |row| {
                Ok(NodeRecord {
                    node_id: row.get(0)?,
                    relative_path: row.get(1)?,
                    parent_node_id: row.get(2)?,
                })
            },
        )
        .optional()
        .context("failed to load persisted node metadata")?
        .ok_or_else(|| anyhow!("node_id `{node_id}` does not exist for plan `{plan_id}`"))
}

pub(crate) fn load_child_nodes(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: Option<&str>,
) -> Result<Vec<NodeRecord>> {
    let sql = match parent_node_id {
        Some(_) => {
            "SELECT node_id, relative_path, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND parent_node_id = ?2
             ORDER BY relative_path, node_id"
        }
        None => {
            "SELECT node_id, relative_path, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND parent_node_id IS NULL
             ORDER BY relative_path, node_id"
        }
    };
    let mut statement = connection
        .prepare(sql)
        .context("failed to prepare child node lookup")?;
    match parent_node_id {
        Some(parent_node_id) => statement
            .query_map(params![plan_id, parent_node_id], |row| {
                Ok(NodeRecord {
                    node_id: row.get(0)?,
                    relative_path: row.get(1)?,
                    parent_node_id: row.get(2)?,
                })
            })
            .context("failed to query child nodes")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read child nodes"),
        None => statement
            .query_map(params![plan_id], |row| {
                Ok(NodeRecord {
                    node_id: row.get(0)?,
                    relative_path: row.get(1)?,
                    parent_node_id: row.get(2)?,
                })
            })
            .context("failed to query child nodes")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read child nodes"),
    }
}

pub(crate) fn load_latest_passed_leaf_gate_summary(
    connection: &Connection,
    plan_id: &str,
    node_id: &str,
) -> Result<Option<LeafGateSummary>> {
    connection
        .query_row(
            "SELECT leaf_gate_run_id, reviewer_role_id, summary
             FROM GEN_PLAN__leaf_gate_runs
             WHERE plan_id = ?1 AND node_id = ?2 AND passed = 1
             ORDER BY CAST(created_at AS INTEGER) DESC, leaf_gate_run_id DESC
             LIMIT 1",
            params![plan_id, node_id],
            |row| {
                Ok(LeafGateSummary {
                    gate_run_id: row.get(0)?,
                    reviewer_role_id: row.get(1)?,
                    summary: row.get(2)?,
                })
            },
        )
        .optional()
        .context("failed to load passed leaf gate summary")
}

fn ensure_node_id_for_path(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
    parent_relative_path: Option<&str>,
) -> Result<String> {
    let requested_parent_node_id = match parent_relative_path {
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

    if let Some(existing) = select_node(connection, plan_id, relative_path)? {
        if let Some(requested_parent_node_id) = requested_parent_node_id.as_deref() {
            if existing.parent_node_id.as_deref() != Some(requested_parent_node_id) {
                return Err(anyhow!(
                    "parent_relative_path conflicts with existing node linkage for `{relative_path}`"
                ));
            }
        }
        return Ok(existing.node_id);
    }

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
            requested_parent_node_id,
            timestamp,
            timestamp,
        ],
    ) {
        if let Some(existing) = select_node(connection, plan_id, relative_path)? {
            if let Some(requested_parent_node_id) = requested_parent_node_id.as_deref() {
                if existing.parent_node_id.as_deref() != Some(requested_parent_node_id) {
                    return Err(anyhow!(
                        "parent_relative_path conflicts with existing node linkage for `{relative_path}`"
                    ));
                }
            }
            return Ok(existing.node_id);
        }
        return Err(error).context("failed to persist node metadata");
    }

    Ok(node_id)
}

fn select_plan(
    connection: &Connection,
    workspace_root: &Path,
    plan_name: &str,
    expected_plan_root: &Path,
) -> Result<Option<PlanRow>> {
    let expected_plan_root = path_string(expected_plan_root);
    let plan = connection
        .query_row(
            "SELECT plan_id, plan_root, plan_status, task_type, project_directory
             FROM GEN_PLAN__plans
             WHERE workspace_root = ?1 AND plan_name = ?2",
            params![workspace_root_string(workspace_root), plan_name],
            |row| {
                Ok(PlanRow {
                    plan_id: row.get(0)?,
                    plan_root: row.get(1)?,
                    plan_status: row.get(2)?,
                    task_type: row.get(3)?,
                    _project_directory: row.get(4)?,
                })
            },
        )
        .optional()
        .context("failed to read persisted plan metadata")?;

    if let Some(plan) = &plan {
        if plan.plan_root != expected_plan_root {
            return Err(anyhow!(
                "persisted plan_root does not match the fixed plan location for `{plan_name}`"
            ));
        }
    }

    Ok(plan)
}

fn select_node(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
) -> Result<Option<NodeRow>> {
    connection
        .query_row(
            "SELECT node_id, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND relative_path = ?2",
            params![plan_id, relative_path],
            |row| {
                Ok(NodeRow {
                    node_id: row.get(0)?,
                    parent_node_id: row.get(1)?,
                })
            },
        )
        .optional()
        .context("failed to read persisted node metadata")
}

fn validate_plan_local_path(label: &str, input: &str) -> Result<String> {
    if input.is_empty() {
        return Err(anyhow!("{label} must not be empty"));
    }

    let path = Path::new(input);
    if path.is_absolute() {
        return Err(anyhow!(
            "{label} must be a plan-local relative path: absolute paths are not allowed"
        ));
    }

    let mut normalized_components = Vec::<OsString>::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized_components.push(component.to_os_string()),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(anyhow!(
                    "{label} must be a normalized plan-local relative path"
                ));
            }
        }
    }

    if normalized_components.is_empty() {
        return Err(anyhow!("{label} must not be empty"));
    }

    let normalized =
        normalized_components
            .into_iter()
            .fold(PathBuf::new(), |mut path, component| {
                path.push(component);
                path
            });
    let normalized = path_string(&normalized);
    if normalized != input {
        return Err(anyhow!(
            "{label} must be a normalized plan-local relative path"
        ));
    }

    Ok(normalized)
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

fn normalize_project_directory(workspace_root: &Path, project_directory: &Path) -> Result<String> {
    if project_directory.as_os_str().is_empty() {
        return Err(anyhow!("project_directory must not be empty"));
    }
    let project_directory = if project_directory.is_absolute() {
        project_directory.to_path_buf()
    } else {
        workspace_root.join(project_directory)
    };
    Ok(path_string(&project_directory))
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
    _project_directory: String,
}

struct NodeRow {
    node_id: String,
    parent_node_id: Option<String>,
}

pub(crate) struct LeafGateSummary {
    pub gate_run_id: String,
    pub reviewer_role_id: String,
    pub summary: String,
}
