use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::{GateSummary, InspectNodeRequest, ListChildrenRequest, NodeKind, Runtime};

use super::rewrite::{RefineChangedFileKind, RefineRewriteResult, RefineStructuralChange};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRefineGateSelectionInputsRequest {
    pub plan_id: String,
    pub rewrite_result: RefineRewriteResult,
    pub stale_result_handoff: Vec<RefineStaleResultHandoff>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateSelectionInputs {
    pub rewrite_result: RefineRewriteResult,
    pub runtime_snapshot: RefineRuntimeNodeSnapshot,
    pub prior_gate_summaries: RefinePriorGateSummaries,
    pub stale_result_handoff: Vec<RefineStaleResultHandoff>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineRuntimeNodeSnapshot {
    pub nodes: Vec<RefineRuntimeNodeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRuntimeNodeSummary {
    pub node_id: String,
    pub relative_path: String,
    pub node_kind: NodeKind,
    pub parent_node_id: Option<String>,
    pub parent_relative_path: Option<String>,
    pub child_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefinePriorGateSummaries {
    pub leaf: Vec<RefinePriorLeafGateSummary>,
    pub frontier: Vec<RefinePriorFrontierGateSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinePriorLeafGateSummary {
    pub node_id: String,
    pub relative_path: String,
    pub summary: GateSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinePriorFrontierGateSummary {
    pub parent_node_id: String,
    pub parent_relative_path: String,
    pub summary: GateSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineStaleResultHandoff {
    pub target_kind: StaleGateTargetKind,
    pub node_id: Option<String>,
    pub relative_path: String,
    pub parent_node_id: Option<String>,
    pub parent_relative_path: Option<String>,
    pub regenerated_child_relative_path: Option<String>,
    pub classification: RefineStaleGateClassification,
    pub invalidation_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineStaleGateClassification {
    Stale,
    NotStale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StaleGateTargetKind {
    Leaf,
    Frontier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefineRuntimeStateLoadError {
    InvalidCanonicalPath {
        relative_path: String,
    },
    MissingTrackedNode {
        relative_path: String,
        source: String,
    },
    MissingParentChildSet {
        parent_relative_path: String,
        source: String,
    },
    StaleHandoffMismatch {
        relative_path: String,
    },
}

impl fmt::Display for RefineRuntimeStateLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RefineRuntimeStateLoadError {}

pub fn build_refine_gate_selection_inputs(
    runtime: &Runtime,
    request: BuildRefineGateSelectionInputsRequest,
) -> Result<RefineGateSelectionInputs, RefineRuntimeStateLoadError> {
    // This runtime-state-loading API uses Runtime::inspect_node and Runtime::list_children.
    // It must not inspect the private runtime database, register nodes, select targets, or run gates.
    // The output is ready for SelectRefineGateTargetsRequest.
    let created_paths = created_pre_registration_paths(&request.rewrite_result);
    let mut load_targets = BTreeMap::<String, Option<String>>::new();
    collect_rewrite_paths(&request.rewrite_result, &created_paths, &mut load_targets)?;
    collect_stale_handoff_paths(
        &request.stale_result_handoff,
        &created_paths,
        &mut load_targets,
    )?;

    let mut snapshot = RefineRuntimeNodeSnapshot::default();
    let mut prior = RefinePriorGateSummaries::default();

    for (relative_path, node_id) in load_targets {
        let inspected = runtime
            .inspect_node(InspectNodeRequest {
                plan_id: request.plan_id.clone(),
                node_id: node_id.clone(),
                relative_path: node_id
                    .is_none()
                    .then(|| relative_path.clone()),
            })
            .map_err(|source| RefineRuntimeStateLoadError::MissingTrackedNode {
                relative_path: relative_path.clone(),
                source: source.to_string(),
            })?;
        let child_relative_paths = if inspected.node_kind == NodeKind::Parent {
            runtime
                .list_children(ListChildrenRequest {
                    plan_id: request.plan_id.clone(),
                    parent_node_id: Some(inspected.node_id.clone()),
                    parent_relative_path: None,
                })
                .map_err(
                    |source| RefineRuntimeStateLoadError::MissingParentChildSet {
                        parent_relative_path: inspected.relative_path.clone(),
                        source: source.to_string(),
                    },
                )?
                .children
                .into_iter()
                .map(|child| child.relative_path)
                .collect()
        } else {
            Vec::new()
        };

        if let Some(summary) = inspected.latest_passed_leaf_gate_summary.clone() {
            prior.leaf.push(RefinePriorLeafGateSummary {
                node_id: inspected.node_id.clone(),
                relative_path: inspected.relative_path.clone(),
                summary,
            });
        }
        if let Some(summary) = inspected.latest_frontier_gate_summary.clone() {
            prior.frontier.push(RefinePriorFrontierGateSummary {
                parent_node_id: inspected.node_id.clone(),
                parent_relative_path: inspected.relative_path.clone(),
                summary,
            });
        }
        snapshot.nodes.push(RefineRuntimeNodeSummary {
            node_id: inspected.node_id,
            relative_path: inspected.relative_path,
            node_kind: inspected.node_kind,
            parent_node_id: inspected.parent_node_id,
            parent_relative_path: inspected.parent_relative_path,
            child_relative_paths,
        });
    }

    validate_stale_handoff(&request.stale_result_handoff, &snapshot, &prior)?;

    Ok(RefineGateSelectionInputs {
        // returns it unchanged as inputs.rewrite_result
        rewrite_result: request.rewrite_result,
        runtime_snapshot: snapshot,
        prior_gate_summaries: prior,
        stale_result_handoff: request.stale_result_handoff,
    })
}

fn created_pre_registration_paths(rewrite_result: &RefineRewriteResult) -> BTreeSet<String> {
    rewrite_result
        .changed_files
        .iter()
        .filter(|file| file.change_kind == RefineChangedFileKind::Created)
        .map(|file| file.relative_path.clone())
        .collect()
}

fn collect_rewrite_paths(
    rewrite_result: &RefineRewriteResult,
    created_paths: &BTreeSet<String>,
    load_targets: &mut BTreeMap<String, Option<String>>,
) -> Result<(), RefineRuntimeStateLoadError> {
    for file in &rewrite_result.changed_files {
        validate_path(&file.relative_path)?;
        // RefineChangedFileKind::Created: newly created pre-registration nodes are not treated as missing tracked runtime state.
        if file.change_kind != RefineChangedFileKind::Created {
            load_targets.insert(file.relative_path.clone(), file.node_id.clone());
        }
    }
    for change in &rewrite_result.structural_changes {
        collect_structural_change(change, created_paths, load_targets)?;
    }
    for stale in &rewrite_result.stale_nodes {
        validate_path(&stale.relative_path)?;
        load_targets.insert(stale.relative_path.clone(), stale.node_id.clone());
    }
    for invalidation in &rewrite_result.context_invalidations {
        validate_path(&invalidation.relative_path)?;
        load_targets.insert(
            invalidation.relative_path.clone(),
            invalidation.node_id.clone(),
        );
    }
    for unchanged in &rewrite_result.unchanged_nodes {
        validate_path(&unchanged.relative_path)?;
        load_targets.insert(unchanged.relative_path.clone(), unchanged.node_id.clone());
    }
    Ok(())
}

fn collect_structural_change(
    change: &RefineStructuralChange,
    created_paths: &BTreeSet<String>,
    load_targets: &mut BTreeMap<String, Option<String>>,
) -> Result<(), RefineRuntimeStateLoadError> {
    validate_path(&change.parent_relative_path)?;
    // Structural changes for newly created parents follow the same pre-registration exemption.
    // Do not call Runtime::inspect_node or Runtime::list_children for that parent before registration;
    // omits that parent from RefineRuntimeNodeSnapshot.
    if !(change.parent_node_id.is_none() && created_paths.contains(&change.parent_relative_path)) {
        load_targets.insert(
            change.parent_relative_path.clone(),
            change.parent_node_id.clone(),
        );
    }
    for child in change
        .added_child_relative_paths
        .iter()
        .chain(change.removed_child_relative_paths.iter())
    {
        validate_path(child)?;
    }
    Ok(())
}

fn collect_stale_handoff_paths(
    handoff: &[RefineStaleResultHandoff],
    created_paths: &BTreeSet<String>,
    load_targets: &mut BTreeMap<String, Option<String>>,
) -> Result<(), RefineRuntimeStateLoadError> {
    for item in handoff {
        validate_path(&item.relative_path)?;
        if !created_paths.contains(&item.relative_path) {
            load_targets.insert(item.relative_path.clone(), item.node_id.clone());
        }
        if let Some(parent) = &item.parent_relative_path {
            validate_path(parent)?;
            if !created_paths.contains(parent) {
                load_targets.insert(parent.clone(), item.parent_node_id.clone());
            }
        }
        if let Some(child) = &item.regenerated_child_relative_path {
            validate_path(child)?;
        }
    }
    Ok(())
}

fn validate_stale_handoff(
    handoff: &[RefineStaleResultHandoff],
    snapshot: &RefineRuntimeNodeSnapshot,
    prior: &RefinePriorGateSummaries,
) -> Result<(), RefineRuntimeStateLoadError> {
    for item in handoff {
        if item.classification == RefineStaleGateClassification::NotStale {
            continue;
        }
        match item.target_kind {
            StaleGateTargetKind::Leaf => {
                let node_id = item.node_id.as_deref();
                let snapshot_match = snapshot.nodes.iter().any(|node| {
                    node.node_kind == NodeKind::Leaf
                        && node.relative_path == item.relative_path
                        && node_id.is_none_or(|id| id == node.node_id)
                });
                let prior_match = prior.leaf.iter().any(|summary| {
                    summary.relative_path == item.relative_path
                        && node_id.is_none_or(|id| id == summary.node_id)
                });
                if !snapshot_match || !prior_match {
                    return Err(RefineRuntimeStateLoadError::StaleHandoffMismatch {
                        relative_path: item.relative_path.clone(),
                    });
                }
            }
            StaleGateTargetKind::Frontier => {
                let parent_path = item
                    .parent_relative_path
                    .as_deref()
                    .unwrap_or(&item.relative_path);
                let parent_id = item.parent_node_id.as_deref().or(item.node_id.as_deref());
                let snapshot_match = snapshot.nodes.iter().any(|node| {
                    node.node_kind == NodeKind::Parent
                        && node.relative_path == parent_path
                        && parent_id.is_none_or(|id| id == node.node_id)
                });
                let prior_match = prior.frontier.iter().any(|summary| {
                    summary.parent_relative_path == parent_path
                        && parent_id.is_none_or(|id| id == summary.parent_node_id)
                });
                if !snapshot_match || !prior_match {
                    return Err(RefineRuntimeStateLoadError::StaleHandoffMismatch {
                        relative_path: parent_path.to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_path(relative_path: &str) -> Result<(), RefineRuntimeStateLoadError> {
    let path = Path::new(relative_path);
    if relative_path.is_empty()
        || path.is_absolute()
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return Err(RefineRuntimeStateLoadError::InvalidCanonicalPath {
            relative_path: relative_path.to_owned(),
        });
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(RefineRuntimeStateLoadError::InvalidCanonicalPath {
                relative_path: relative_path.to_owned(),
            });
        }
    }
    Ok(())
}
