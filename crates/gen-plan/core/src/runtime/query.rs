use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::child_links::parse_child_node_link_paths;
use super::db::{PROJECT_DIRECTORY_SOURCE_BACKFILLED_LEGACY, PROJECT_DIRECTORY_SOURCE_EXPLICIT};
use crate::{
    EnsureNodeIdRequest, EnsureNodeIdResponse, EnsurePlanRequest, EnsurePlanResponse, GateSummary,
    InspectNodeRequest, InspectNodeResponse, ListChildrenRequest, ListChildrenResponse, NodeKind,
    NodeSummary, OpenPlanRequest, OpenPlanResponse, ReconcileParentChildLinksRequest,
    ReconcileParentChildLinksResponse,
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
    pub node_kind: NodeKind,
    pub parent_node_id: Option<String>,
}

#[derive(Debug, Clone)]
struct NodeRow {
    node_id: String,
    relative_path: String,
    node_kind: NodeKind,
    parent_node_id: Option<String>,
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
        if existing.project_directory != project_directory {
            if existing.project_directory_source == PROJECT_DIRECTORY_SOURCE_BACKFILLED_LEGACY {
                repair_plan_project_directory(
                    connection,
                    &existing.plan_id,
                    &project_directory,
                    current_timestamp()?,
                )?;
            } else if existing.project_directory_source == PROJECT_DIRECTORY_SOURCE_EXPLICIT {
                return Err(anyhow!(
                    "persisted project_directory for existing plan `{plan_name}` is `{}` and cannot be redirected to `{project_directory}`",
                    existing.project_directory
                ));
            } else {
                return Err(anyhow!(
                    "persisted project_directory_source `{}` for existing plan `{plan_name}` is invalid",
                    existing.project_directory_source
                ));
            }
        }
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
                project_directory_source,
                task_type,
                plan_status,
                created_at,
                updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                plan_id,
                workspace_root,
                plan_name,
                plan_root,
                project_directory,
                PROJECT_DIRECTORY_SOURCE_EXPLICIT,
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

pub(crate) fn inspect_node(
    connection: &Connection,
    request: InspectNodeRequest,
) -> Result<InspectNodeResponse> {
    let InspectNodeRequest {
        plan_id,
        node_id,
        relative_path,
    } = request;
    let node = match (node_id, relative_path) {
        (Some(node_id), None) => load_node_record(connection, &plan_id, &node_id)?,
        (None, Some(relative_path)) => {
            let relative_path = validate_plan_local_path("relative_path", &relative_path)?;
            validate_registered_node_path("relative_path", &relative_path)?;
            load_node_record_by_relative_path(connection, &plan_id, &relative_path)?.ok_or_else(
                || anyhow!("relative_path `{relative_path}` does not exist for plan `{plan_id}`"),
            )?
        }
        (Some(_), Some(_)) => {
            return Err(anyhow!(
                "inspect-node requires exactly one selector: either node_id or relative_path"
            ));
        }
        (None, None) => {
            return Err(anyhow!(
                "inspect-node requires exactly one selector: either node_id or relative_path"
            ));
        }
    };

    let children = load_child_nodes(connection, &plan_id, Some(&node.node_id))?
        .into_iter()
        .map(node_record_to_summary)
        .collect();
    let parent_relative_path = match node.parent_node_id.as_deref() {
        Some(parent_node_id) => {
            Some(load_node_record(connection, &plan_id, parent_node_id)?.relative_path)
        }
        None => None,
    };

    Ok(InspectNodeResponse {
        node_id: node.node_id.clone(),
        relative_path: node.relative_path.clone(),
        node_kind: node.node_kind,
        parent_node_id: node.parent_node_id.clone(),
        parent_relative_path,
        children,
        latest_passed_leaf_gate_summary: load_latest_passed_leaf_gate_summary(
            connection,
            &plan_id,
            &node.node_id,
        )?
        .map(leaf_gate_summary_to_api),
        latest_frontier_gate_summary: load_latest_frontier_gate_summary(
            connection,
            &plan_id,
            &node.node_id,
        )?
        .map(frontier_gate_summary_to_api),
    })
}

pub(crate) fn list_children(
    connection: &Connection,
    request: ListChildrenRequest,
) -> Result<ListChildrenResponse> {
    let ListChildrenRequest {
        plan_id,
        parent_node_id,
        parent_relative_path,
    } = request;
    let parent = match (parent_node_id, parent_relative_path) {
        (Some(parent_node_id), None) => load_node_record(connection, &plan_id, &parent_node_id)?,
        (None, Some(parent_relative_path)) => {
            let parent_relative_path =
                validate_plan_local_path("parent_relative_path", &parent_relative_path)?;
            require_parent_node_path("parent_relative_path", &parent_relative_path)?;
            load_node_record_by_relative_path(connection, &plan_id, &parent_relative_path)?
                .ok_or_else(|| anyhow!("parent_relative_path `{parent_relative_path}` does not exist for plan `{plan_id}`"))?
        }
        (Some(_), Some(_)) => {
            return Err(anyhow!(
                "list-children requires exactly one selector: either parent_node_id or parent_relative_path"
            ));
        }
        (None, None) => {
            return Err(anyhow!(
                "list-children requires exactly one selector: either parent_node_id or parent_relative_path"
            ));
        }
    };
    if parent.node_kind != NodeKind::Parent {
        return Err(anyhow!(
            "list-children requires a parent node target, but `{}` is `{}`",
            parent.relative_path,
            parent.node_kind.as_str()
        ));
    }

    Ok(ListChildrenResponse {
        parent_node_id: parent.node_id.clone(),
        parent_relative_path: parent.relative_path.clone(),
        children: load_child_nodes(connection, &plan_id, Some(&parent.node_id))?
            .into_iter()
            .map(node_record_to_summary)
            .collect(),
    })
}

pub(crate) fn reconcile_parent_child_links(
    connection: &Connection,
    request: ReconcileParentChildLinksRequest,
) -> Result<ReconcileParentChildLinksResponse> {
    let ReconcileParentChildLinksRequest {
        plan_id,
        parent_relative_path,
    } = request;
    let parent_relative_path =
        validate_plan_local_path("parent_relative_path", &parent_relative_path)?;
    require_parent_node_path("parent_relative_path", &parent_relative_path)?;
    let plan = load_gate_plan_context(connection, &plan_id)?;
    let parent = load_node_record_by_relative_path(connection, &plan_id, &parent_relative_path)?
        .ok_or_else(|| {
            anyhow!(
                "parent_relative_path `{parent_relative_path}` does not exist for plan `{plan_id}`"
            )
        })?;
    require_parent_node_record(&parent)?;
    ensure_plan_markdown_file_exists(&plan.plan_root, &parent.relative_path)?;
    let parent_markdown = std::fs::read_to_string(plan.plan_root.join(&parent.relative_path))
        .with_context(|| {
            format!(
                "failed to read plan markdown {}",
                plan.plan_root.join(&parent.relative_path).display()
            )
        })?;
    let linked_child_relative_paths =
        parse_child_node_link_paths(&parent.relative_path, &parent_markdown)?;
    let linked_child_set = linked_child_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();

    let mut validated_linked_children = Vec::new();
    for child_relative_path in &linked_child_relative_paths {
        let child_kind = validate_registered_node_path("child_relative_path", child_relative_path)?;
        validate_direct_child_relationship(
            child_relative_path,
            Some(&parent.relative_path),
            child_kind,
        )?;
        ensure_plan_markdown_file_exists(&plan.plan_root, child_relative_path)?;
        let child = load_node_record_by_relative_path(connection, &plan_id, child_relative_path)?
            .ok_or_else(|| {
                anyhow!(
                    "linked child_relative_path `{child_relative_path}` must be tracked before reconciling parent `{}`",
                    parent.relative_path
                )
        })?;
        validate_direct_child_relationship(
            &child.relative_path,
            Some(&parent.relative_path),
            child.node_kind,
        )?;
        validated_linked_children.push(child);
    }
    reject_still_linked_reparent_conflicts(
        connection,
        &plan,
        &plan_id,
        &parent,
        &validated_linked_children,
    )?;

    let timestamp = current_timestamp()?;

    let current_children = load_child_nodes(connection, &plan_id, Some(&parent.node_id))?;
    let mut detached_child_relative_paths = Vec::new();
    for child in current_children {
        if linked_child_set.contains(child.relative_path.as_str()) {
            continue;
        }
        set_node_parent(
            connection,
            &plan_id,
            &child.node_id,
            None,
            timestamp.as_str(),
        )?;
        detached_child_relative_paths.push(child.relative_path);
    }

    let mut attached_child_relative_paths = Vec::new();
    for child in validated_linked_children {
        if child.parent_node_id.as_deref() == Some(parent.node_id.as_str()) {
            continue;
        }
        set_node_parent(
            connection,
            &plan_id,
            &child.node_id,
            Some(parent.node_id.as_str()),
            timestamp.as_str(),
        )?;
        attached_child_relative_paths.push(child.relative_path);
    }

    Ok(ReconcileParentChildLinksResponse {
        parent_node_id: parent.node_id,
        parent_relative_path: parent.relative_path,
        linked_child_relative_paths,
        attached_child_relative_paths,
        detached_child_relative_paths,
    })
}

fn reject_still_linked_reparent_conflicts(
    connection: &Connection,
    plan: &GatePlanContext,
    plan_id: &str,
    parent: &NodeRecord,
    linked_children: &[NodeRecord],
) -> Result<()> {
    for child in linked_children {
        let Some(existing_parent_node_id) = child.parent_node_id.as_deref() else {
            continue;
        };
        if existing_parent_node_id == parent.node_id {
            continue;
        }
        let existing_parent = load_node_record(connection, plan_id, existing_parent_node_id)
            .with_context(|| {
                format!(
                    "failed to inspect existing parent `{existing_parent_node_id}` for linked child `{}`",
                    child.relative_path
                )
            })?;
        require_parent_node_record(&existing_parent)?;
        ensure_plan_markdown_file_exists(&plan.plan_root, &existing_parent.relative_path)?;
        let existing_parent_path = plan.plan_root.join(&existing_parent.relative_path);
        let existing_parent_markdown = std::fs::read_to_string(&existing_parent_path)
            .with_context(|| {
                format!(
                    "failed to read plan markdown {}",
                    existing_parent_path.display()
                )
            })?;
        let existing_parent_links =
            parse_child_node_link_paths(&existing_parent.relative_path, &existing_parent_markdown)?;
        if existing_parent_links
            .iter()
            .any(|relative_path| relative_path == &child.relative_path)
        {
            return Err(anyhow!(
                "linked child_relative_path `{}` is still linked from existing parent `{}`; reconcile that parent before attaching it to `{}`",
                child.relative_path,
                existing_parent.relative_path,
                parent.relative_path
            ));
        }
    }
    Ok(())
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
    let row = connection
        .query_row(
            "SELECT node_id, relative_path, node_kind, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND node_id = ?2",
            params![plan_id, node_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .context("failed to load persisted node metadata")?
        .ok_or_else(|| anyhow!("node_id `{node_id}` does not exist for plan `{plan_id}`"))?;
    Ok(NodeRecord {
        node_id: row.0,
        relative_path: row.1,
        node_kind: parse_node_kind(row.2)?,
        parent_node_id: row.3,
    })
}

pub(crate) fn load_child_nodes(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: Option<&str>,
) -> Result<Vec<NodeRecord>> {
    let sql = match parent_node_id {
        Some(_) => {
            "SELECT node_id, relative_path, node_kind, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND parent_node_id = ?2
             ORDER BY relative_path, node_id"
        }
        None => {
            "SELECT node_id, relative_path, node_kind, parent_node_id
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
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .context("failed to query child nodes")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read child nodes")?
            .into_iter()
            .map(|(node_id, relative_path, node_kind, parent_node_id)| {
                Ok(NodeRecord {
                    node_id,
                    relative_path,
                    node_kind: parse_node_kind(node_kind)?,
                    parent_node_id,
                })
            })
            .collect(),
        None => statement
            .query_map(params![plan_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .context("failed to query child nodes")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read child nodes")?
            .into_iter()
            .map(|(node_id, relative_path, node_kind, parent_node_id)| {
                Ok(NodeRecord {
                    node_id,
                    relative_path,
                    node_kind: parse_node_kind(node_kind)?,
                    parent_node_id,
                })
            })
            .collect(),
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

pub(crate) fn load_latest_frontier_gate_summary(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: &str,
) -> Result<Option<FrontierGateSummary>> {
    connection
        .query_row(
            "SELECT frontier_gate_run_id, reviewer_role_id, summary
             FROM GEN_PLAN__frontier_gate_runs
             WHERE plan_id = ?1 AND parent_node_id = ?2
             ORDER BY CAST(created_at AS INTEGER) DESC, frontier_gate_run_id DESC
             LIMIT 1",
            params![plan_id, parent_node_id],
            |row| {
                Ok(FrontierGateSummary {
                    gate_run_id: row.get(0)?,
                    reviewer_role_id: row.get(1)?,
                    summary: row.get(2)?,
                })
            },
        )
        .optional()
        .context("failed to load latest frontier gate summary")
}

pub(crate) fn require_leaf_node_record(node: &NodeRecord) -> Result<()> {
    validate_registered_node_path("relative_path", &node.relative_path)?;
    if node.node_kind != NodeKind::Leaf {
        return Err(anyhow!(
            "leaf review requires a leaf node target, but `{}` is `{}`",
            node.relative_path,
            node.node_kind.as_str()
        ));
    }
    Ok(())
}

pub(crate) fn require_parent_node_record(node: &NodeRecord) -> Result<()> {
    require_parent_node_path("relative_path", &node.relative_path)?;
    if node.node_kind != NodeKind::Parent {
        return Err(anyhow!(
            "frontier review requires a parent node target, but `{}` is `{}`",
            node.relative_path,
            node.node_kind.as_str()
        ));
    }
    Ok(())
}

pub(crate) fn ensure_plan_markdown_file_exists(
    plan_root: &Path,
    relative_path: &str,
) -> Result<()> {
    let full_path = plan_root.join(relative_path);
    let metadata = std::fs::metadata(&full_path)
        .with_context(|| format!("missing plan markdown {}", full_path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "expected plan markdown file at {}, but found a non-file target",
            full_path.display()
        ));
    }
    Ok(())
}

pub(crate) fn ensure_parent_child_runtime_coherence(
    connection: &Connection,
    plan_id: &str,
    parent: &NodeRecord,
) -> Result<()> {
    require_parent_node_record(parent)?;
    for child in load_child_nodes(connection, plan_id, Some(&parent.node_id))? {
        validate_direct_child_relationship(
            &child.relative_path,
            Some(&parent.relative_path),
            child.node_kind,
        )?;
    }
    Ok(())
}

fn set_node_parent(
    connection: &Connection,
    plan_id: &str,
    node_id: &str,
    parent_node_id: Option<&str>,
    updated_at: &str,
) -> Result<()> {
    connection
        .execute(
            "UPDATE GEN_PLAN__nodes
             SET parent_node_id = ?1,
                 updated_at = ?2
             WHERE plan_id = ?3 AND node_id = ?4",
            params![parent_node_id, updated_at, plan_id, node_id],
        )
        .context("failed to update node parent linkage")?;
    Ok(())
}

fn ensure_node_id_for_path(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
    parent_relative_path: Option<&str>,
) -> Result<String> {
    let requested_node_kind = validate_registration_request(relative_path, parent_relative_path)?;
    let requested_parent = match parent_relative_path {
        Some(parent_relative_path) if parent_relative_path == relative_path => {
            return Err(anyhow!(
                "parent_relative_path must differ from relative_path for `{relative_path}`"
            ));
        }
        Some(parent_relative_path) => {
            let parent = load_node_record_by_relative_path(connection, plan_id, parent_relative_path)?
                .ok_or_else(|| {
                    anyhow!(
                        "parent_relative_path `{parent_relative_path}` must reference an existing tracked parent node before registering `{relative_path}`"
                    )
                })?;
            if parent.node_kind != NodeKind::Parent {
                return Err(anyhow!(
                    "parent_relative_path `{parent_relative_path}` must point to a tracked parent node, but it is `{}`",
                    parent.node_kind.as_str()
                ));
            }
            Some(parent)
        }
        None => None,
    };

    if let Some(existing) = select_node_by_relative_path(connection, plan_id, relative_path)? {
        ensure_existing_node_matches_request(
            connection,
            plan_id,
            relative_path,
            requested_node_kind,
            requested_parent.as_ref(),
            &existing,
        )?;
        return Ok(existing.node_id);
    }

    let node_id = Uuid::new_v4().to_string();
    let node_name = node_name(relative_path);
    let timestamp = current_timestamp()?;
    let requested_parent_node_id = requested_parent.as_ref().map(|node| node.node_id.as_str());

    if let Err(error) = connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id,
            node_id,
            relative_path,
            node_name,
            node_kind,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            plan_id,
            node_id,
            relative_path,
            node_name,
            requested_node_kind.as_str(),
            requested_parent_node_id,
            timestamp,
            timestamp,
        ],
    ) {
        if let Some(existing) = select_node_by_relative_path(connection, plan_id, relative_path)? {
            ensure_existing_node_matches_request(
                connection,
                plan_id,
                relative_path,
                requested_node_kind,
                requested_parent.as_ref(),
                &existing,
            )?;
            return Ok(existing.node_id);
        }
        return Err(error).context("failed to persist node metadata");
    }

    Ok(node_id)
}

fn ensure_existing_node_matches_request(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
    requested_node_kind: NodeKind,
    requested_parent: Option<&NodeRecord>,
    existing: &NodeRow,
) -> Result<()> {
    if existing.node_kind != requested_node_kind {
        return Err(anyhow!(
            "requested target path `{relative_path}` resolves to node kind `{}`, but existing node `{}` is stored as `{}`",
            requested_node_kind.as_str(),
            existing.node_id,
            existing.node_kind.as_str()
        ));
    }

    let requested_parent_node_id = requested_parent.map(|parent| parent.node_id.as_str());
    let existing_parent_node_id = existing.parent_node_id.as_deref();
    if existing_parent_node_id != requested_parent_node_id {
        let existing_parent_relative_path = match existing.parent_node_id.as_deref() {
            Some(parent_node_id) => Some(
                connection
                    .query_row(
                        "SELECT relative_path FROM GEN_PLAN__nodes WHERE plan_id = ?1 AND node_id = ?2",
                        params![plan_id, parent_node_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .context("failed to read conflicting parent relative path")?
                    .unwrap_or_else(|| "<unknown-parent>".to_owned()),
            ),
            None => None,
        };
        return Err(anyhow!(
            "parent_relative_path conflicts with existing node linkage for `{relative_path}`: requested_parent_relative_path={:?}, existing_node_id=`{}`, existing_parent_node_id={:?}, existing_parent_relative_path={:?}, existing_relative_path=`{}`",
            requested_parent.map(|parent| parent.relative_path.as_str()),
            existing.node_id,
            existing.parent_node_id,
            existing_parent_relative_path,
            existing.relative_path,
        ));
    }

    Ok(())
}

fn validate_registration_request(
    relative_path: &str,
    parent_relative_path: Option<&str>,
) -> Result<NodeKind> {
    let node_kind = validate_registered_node_path("relative_path", relative_path)?;
    match parent_relative_path {
        Some(parent_relative_path) => {
            require_parent_node_path("parent_relative_path", parent_relative_path)?;
            validate_direct_child_relationship(
                relative_path,
                Some(parent_relative_path),
                node_kind,
            )?;
            Ok(node_kind)
        }
        None => {
            let components = Path::new(relative_path).components().count();
            match (node_kind, components) {
                (NodeKind::Leaf, 1) | (NodeKind::Parent, 2) => Ok(node_kind),
                (NodeKind::Leaf, _) => Err(anyhow!(
                    "relative_path `{relative_path}` is a nested leaf and requires parent_relative_path pointing to the tracked parent markdown path"
                )),
                (NodeKind::Parent, _) => Err(anyhow!(
                    "relative_path `{relative_path}` is a nested parent and requires parent_relative_path pointing to the tracked parent markdown path"
                )),
            }
        }
    }
}

fn validate_direct_child_relationship(
    relative_path: &str,
    parent_relative_path: Option<&str>,
    node_kind: NodeKind,
) -> Result<()> {
    let Some(parent_relative_path) = parent_relative_path else {
        return Ok(());
    };
    let parent_dir = Path::new(parent_relative_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let child_relative = Path::new(relative_path)
        .strip_prefix(parent_dir)
        .map_err(|_| {
            anyhow!(
                "relative_path `{relative_path}` must stay within the direct child scope of parent_relative_path `{parent_relative_path}`"
            )
        })?;
    let child_components: Vec<_> = child_relative.components().collect();
    let expected_components = match node_kind {
        NodeKind::Leaf => 1,
        NodeKind::Parent => 2,
    };
    if child_components.len() != expected_components {
        return Err(anyhow!(
            "relative_path `{relative_path}` must be a direct child of parent_relative_path `{parent_relative_path}`"
        ));
    }
    Ok(())
}

fn repair_plan_project_directory(
    connection: &Connection,
    plan_id: &str,
    project_directory: &str,
    updated_at: String,
) -> Result<()> {
    connection
        .execute(
            "UPDATE GEN_PLAN__plans
             SET project_directory = ?1,
                 project_directory_source = ?2,
                 updated_at = ?3
             WHERE plan_id = ?4",
            params![
                project_directory,
                PROJECT_DIRECTORY_SOURCE_EXPLICIT,
                updated_at,
                plan_id
            ],
        )
        .context("failed to repair persisted project_directory for existing plan")?;
    Ok(())
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
            "SELECT plan_id, plan_root, plan_status, task_type, project_directory, project_directory_source
             FROM GEN_PLAN__plans
             WHERE workspace_root = ?1 AND plan_name = ?2",
            params![workspace_root_string(workspace_root), plan_name],
            |row| {
                Ok(PlanRow {
                    plan_id: row.get(0)?,
                    plan_root: row.get(1)?,
                    plan_status: row.get(2)?,
                    task_type: row.get(3)?,
                    project_directory: row.get(4)?,
                    project_directory_source: row.get(5)?,
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

fn select_node_by_relative_path(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
) -> Result<Option<NodeRow>> {
    let row = connection
        .query_row(
            "SELECT node_id, relative_path, node_kind, parent_node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND relative_path = ?2",
            params![plan_id, relative_path],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .context("failed to read persisted node metadata")?;
    row.map(|(node_id, relative_path, node_kind, parent_node_id)| {
        Ok(NodeRow {
            node_id,
            relative_path,
            node_kind: parse_node_kind(node_kind)?,
            parent_node_id,
        })
    })
    .transpose()
}

fn load_node_record_by_relative_path(
    connection: &Connection,
    plan_id: &str,
    relative_path: &str,
) -> Result<Option<NodeRecord>> {
    select_node_by_relative_path(connection, plan_id, relative_path).map(|node| {
        node.map(|node| NodeRecord {
            node_id: node.node_id,
            relative_path: node.relative_path,
            node_kind: node.node_kind,
            parent_node_id: node.parent_node_id,
        })
    })
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

pub(crate) fn validate_registered_node_path(label: &str, relative_path: &str) -> Result<NodeKind> {
    let path = Path::new(relative_path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("{label} must point to a canonical markdown file path"))?;
    let stem = file_name.strip_suffix(".md").ok_or_else(|| {
        anyhow!("{label} must point to a canonical markdown file path ending in `.md`")
    })?;
    if stem.is_empty() {
        return Err(anyhow!(
            "{label} must point to a canonical markdown file path with a non-empty stem"
        ));
    }
    let components: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(component) => component.to_str(),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        return Err(anyhow!(
            "{label} must point to a canonical markdown file path"
        ));
    }

    let parent_dir_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    if parent_dir_name == Some(stem) {
        Ok(NodeKind::Parent)
    } else {
        Ok(NodeKind::Leaf)
    }
}

pub(crate) fn require_parent_node_path(label: &str, relative_path: &str) -> Result<()> {
    if validate_registered_node_path(label, relative_path)? != NodeKind::Parent {
        return Err(anyhow!(
            "{label} must point to a canonical parent markdown path such as `scope/scope.md`"
        ));
    }
    Ok(())
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

fn parse_node_kind(value: String) -> Result<NodeKind> {
    match value.as_str() {
        "parent" => Ok(NodeKind::Parent),
        "leaf" => Ok(NodeKind::Leaf),
        other => Err(anyhow!("invalid persisted node_kind `{other}`")),
    }
}

fn node_record_to_summary(node: NodeRecord) -> NodeSummary {
    NodeSummary {
        node_id: node.node_id,
        relative_path: node.relative_path,
        node_kind: node.node_kind,
        parent_node_id: node.parent_node_id,
    }
}

fn leaf_gate_summary_to_api(summary: LeafGateSummary) -> GateSummary {
    GateSummary {
        gate_run_id: summary.gate_run_id,
        reviewer_role_id: summary.reviewer_role_id,
        summary: summary.summary,
    }
}

fn frontier_gate_summary_to_api(summary: FrontierGateSummary) -> GateSummary {
    GateSummary {
        gate_run_id: summary.gate_run_id,
        reviewer_role_id: summary.reviewer_role_id,
        summary: summary.summary,
    }
}

struct PlanRow {
    plan_id: String,
    plan_root: String,
    plan_status: String,
    task_type: String,
    project_directory: String,
    project_directory_source: String,
}

pub(crate) struct LeafGateSummary {
    pub gate_run_id: String,
    pub reviewer_role_id: String,
    pub summary: String,
}

pub(crate) struct FrontierGateSummary {
    pub gate_run_id: String,
    pub reviewer_role_id: String,
    pub summary: String,
}
