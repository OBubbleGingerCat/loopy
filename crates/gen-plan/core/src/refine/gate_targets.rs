use std::collections::HashSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::{GateSummary, NodeKind};

use super::gate_registration::{
    RefineFrontierRegistrationCandidate, RefineGateTargetReason, RefineLeafRegistrationCandidate,
    RefineParentRegistrationCandidate, RegisterRefineGateTargetsRequest,
};
use super::rewrite::{RefineChangedFileKind, RefineRewriteResult, RefineStructuralChangeKind};
use super::runtime_state::{
    RefinePriorGateSummaries, RefineRuntimeNodeSnapshot, RefineRuntimeNodeSummary,
    RefineStaleGateClassification, RefineStaleResultHandoff, StaleGateTargetKind,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectRefineGateTargetsRequest {
    pub plan_id: String,
    pub rewrite_result: RefineRewriteResult,
    pub runtime_snapshot: RefineRuntimeNodeSnapshot,
    pub prior_gate_summaries: RefinePriorGateSummaries,
    pub stale_result_handoff: Vec<RefineStaleResultHandoff>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineGateTargetSelection {
    pub leaf_targets: Vec<RefineLeafGateTarget>,
    pub frontier_targets: Vec<RefineFrontierGateTarget>,
    pub stale_leaf_approvals: Vec<StaleGateApproval>,
    pub stale_frontier_approvals: Vec<StaleGateApproval>,
    pub expected_target_summary: ExpectedGateTargetSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineLeafGateTarget {
    pub node_id: Option<String>,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineFrontierGateTarget {
    pub parent_node_id: Option<String>,
    pub parent_relative_path: String,
    pub changed_child_relative_paths: Vec<String>,
    pub removed_child_relative_paths: Vec<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleGateApproval {
    pub target_kind: StaleGateTargetKind,
    pub node_id: String,
    pub relative_path: String,
    pub gate_run_id: String,
    pub stale_reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExpectedGateTargetSummary {
    pub leaf_targets: Vec<ExpectedLeafGateTargetSummary>,
    pub frontier_targets: Vec<ExpectedFrontierGateTargetSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedLeafGateTargetSummary {
    pub node_id: Option<String>,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFrontierGateTargetSummary {
    pub parent_node_id: Option<String>,
    pub parent_relative_path: String,
    pub changed_child_relative_paths: Vec<String>,
    pub removed_child_relative_paths: Vec<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

impl RefineGateTargetSelection {
    pub fn to_registration_request(&self, plan_id: String) -> RegisterRefineGateTargetsRequest {
        let known_parent_paths = known_parent_paths(self);
        RegisterRefineGateTargetsRequest {
            plan_id,
            parent_candidates: self
                .frontier_targets
                .iter()
                .filter(|target| {
                    target.parent_node_id.is_none()
                        || target.reasons.contains(&RefineGateTargetReason::NewParent)
                })
                .map(|target| RefineParentRegistrationCandidate {
                    relative_path: target.parent_relative_path.clone(),
                    parent_relative_path: ancestor_parent_self_path(
                        &target.parent_relative_path,
                        &known_parent_paths,
                    ),
                    reasons: target.reasons.clone(),
                })
                .collect(),
            leaf_candidates: self
                .leaf_targets
                .iter()
                .map(|target| RefineLeafRegistrationCandidate {
                    relative_path: target.relative_path.clone(),
                    parent_relative_path: target.parent_relative_path.clone(),
                    reasons: target.reasons.clone(),
                })
                .collect(),
            frontier_candidates: self
                .frontier_targets
                .iter()
                .map(|target| RefineFrontierRegistrationCandidate {
                    parent_relative_path: target.parent_relative_path.clone(),
                    changed_child_relative_paths: target.changed_child_relative_paths.clone(),
                    removed_child_relative_paths: target.removed_child_relative_paths.clone(),
                    reasons: target.reasons.clone(),
                })
                .collect(),
        }
    }
}

pub fn select_refine_gate_targets(
    request: SelectRefineGateTargetsRequest,
) -> RefineGateTargetSelection {
    // select_refine_gate_targets runs after rewrite application and before register_refine_gate_targets.
    // It consumes shared input types from crates/gen-plan/core/src/refine/runtime_state.rs and
    // does not define or build those shared input types itself.
    // It returns expected targets only, never actual runtime gate pass/fail results.
    let mut selection = RefineGateTargetSelection::default();
    let explicitly_removed_paths = request
        .rewrite_result
        .changed_files
        .iter()
        .filter(|file| file.change_kind == RefineChangedFileKind::ExplicitlyRemoved)
        .map(|file| file.relative_path.as_str())
        .collect::<Vec<_>>();

    for file in &request.rewrite_result.changed_files {
        match file.change_kind {
            RefineChangedFileKind::TextUpdated => {
                if is_parent_target_path(&request, &file.relative_path) {
                    if is_link_only_parent_text_update(&request, &file.relative_path) {
                        continue;
                    }
                    upsert_frontier(
                        &mut selection.frontier_targets,
                        &request.runtime_snapshot,
                        &file.relative_path,
                        Vec::new(),
                        Vec::new(),
                        RefineGateTargetReason::ParentContractChanged,
                    );
                    upsert_descendant_leaf_targets(
                        &mut selection.leaf_targets,
                        &request.runtime_snapshot,
                        &file.relative_path,
                        &explicitly_removed_paths,
                        RefineGateTargetReason::ParentContractChanged,
                    );
                } else {
                    upsert_leaf(
                        &mut selection.leaf_targets,
                        &request.runtime_snapshot,
                        &file.relative_path,
                        file.node_id.clone(),
                        None,
                        RefineGateTargetReason::TextChanged,
                    );
                }
            }
            RefineChangedFileKind::Created => {
                if is_parent_target_path(&request, &file.relative_path) {
                    upsert_frontier(
                        &mut selection.frontier_targets,
                        &request.runtime_snapshot,
                        &file.relative_path,
                        Vec::new(),
                        Vec::new(),
                        RefineGateTargetReason::NewParent,
                    );
                } else {
                    upsert_leaf(
                        &mut selection.leaf_targets,
                        &request.runtime_snapshot,
                        &file.relative_path,
                        None,
                        None,
                        RefineGateTargetReason::NewLeaf,
                    );
                }
            }
            RefineChangedFileKind::Regenerated => {
                upsert_leaf(
                    &mut selection.leaf_targets,
                    &request.runtime_snapshot,
                    &file.relative_path,
                    file.node_id.clone(),
                    None,
                    RefineGateTargetReason::RegeneratedLeaf,
                );
            }
            RefineChangedFileKind::StaleMarked | RefineChangedFileKind::ExplicitlyRemoved => {}
        }
    }

    for invalidation in &request.rewrite_result.context_invalidations {
        // context_invalidations are already leaf-scoped target records; must not walk ancestor or parent nodes.
        upsert_leaf(
            &mut selection.leaf_targets,
            &request.runtime_snapshot,
            &invalidation.relative_path,
            invalidation.node_id.clone(),
            None,
            RefineGateTargetReason::ContextInvalidated,
        );
    }

    for change in &request.rewrite_result.structural_changes {
        let reason = match change.change_kind {
            RefineStructuralChangeKind::ChangedChildSet => RefineGateTargetReason::ChangedChildSet,
            RefineStructuralChangeKind::ParentContractChanged => {
                RefineGateTargetReason::ParentContractChanged
            }
        };
        let mut changed_child_relative_paths = Vec::new();
        changed_child_relative_paths.extend(change.added_child_relative_paths.clone());
        changed_child_relative_paths.extend(change.removed_child_relative_paths.clone());
        upsert_frontier(
            &mut selection.frontier_targets,
            &request.runtime_snapshot,
            &change.parent_relative_path,
            changed_child_relative_paths,
            change.removed_child_relative_paths.clone(),
            reason.clone(),
        );
        for child in &change.added_child_relative_paths {
            match find_node(&request.runtime_snapshot, child).map(|node| node.node_kind) {
                Some(NodeKind::Leaf) => {
                    upsert_leaf(
                        &mut selection.leaf_targets,
                        &request.runtime_snapshot,
                        child,
                        None,
                        Some(change.parent_relative_path.clone()),
                        reason.clone(),
                    );
                }
                Some(NodeKind::Parent) => {
                    upsert_descendant_leaf_targets(
                        &mut selection.leaf_targets,
                        &request.runtime_snapshot,
                        child,
                        &[],
                        reason.clone(),
                    );
                }
                None => {}
            }
        }
    }

    apply_stale_handoff(&request, &mut selection);
    selection.expected_target_summary = ExpectedGateTargetSummary {
        leaf_targets: selection
            .leaf_targets
            .iter()
            .map(|target| ExpectedLeafGateTargetSummary {
                node_id: target.node_id.clone(),
                relative_path: target.relative_path.clone(),
                parent_relative_path: target.parent_relative_path.clone(),
                reasons: target.reasons.clone(),
            })
            .collect(),
        frontier_targets: selection
            .frontier_targets
            .iter()
            .map(|target| ExpectedFrontierGateTargetSummary {
                parent_node_id: target.parent_node_id.clone(),
                parent_relative_path: target.parent_relative_path.clone(),
                changed_child_relative_paths: target.changed_child_relative_paths.clone(),
                removed_child_relative_paths: target.removed_child_relative_paths.clone(),
                reasons: target.reasons.clone(),
            })
            .collect(),
    };
    selection
}

fn apply_stale_handoff(
    request: &SelectRefineGateTargetsRequest,
    selection: &mut RefineGateTargetSelection,
) {
    // stale_result_handoff classifications are supplied by the stale policy.
    for handoff in &request.stale_result_handoff {
        if handoff.classification != RefineStaleGateClassification::Stale
            || handoff.invalidation_reason.trim().is_empty()
        {
            continue;
        }
        match handoff.target_kind {
            StaleGateTargetKind::Leaf => {
                // stale leaf handoff selects the leaf as a fresh StaleDescendant leaf target.
                upsert_leaf(
                    &mut selection.leaf_targets,
                    &request.runtime_snapshot,
                    &handoff.relative_path,
                    handoff.node_id.clone(),
                    None,
                    RefineGateTargetReason::StaleDescendant,
                );
                if let Some(prior) = request.prior_gate_summaries.leaf.iter().find(|prior| {
                    prior.relative_path == handoff.relative_path
                        && handoff
                            .node_id
                            .as_deref()
                            .map_or(true, |id| id == prior.node_id)
                }) {
                    let stale_reasons =
                        reasons_for_leaf(&selection.leaf_targets, &handoff.relative_path);
                    selection.stale_leaf_approvals.push(StaleGateApproval {
                        target_kind: StaleGateTargetKind::Leaf,
                        node_id: prior.node_id.clone(),
                        relative_path: prior.relative_path.clone(),
                        gate_run_id: prior.summary.gate_run_id.clone(),
                        stale_reasons,
                    });
                }
            }
            StaleGateTargetKind::Frontier => {
                // node_id is always the matched parent_node_id; relative_path is always the matched parent_relative_path.
                let parent_relative_path = handoff
                    .parent_relative_path
                    .clone()
                    .unwrap_or_else(|| handoff.relative_path.clone());
                let changed_child_relative_paths = handoff
                    .regenerated_child_relative_path
                    .clone()
                    .into_iter()
                    .collect::<Vec<_>>();
                // regenerated_child_relative_path is reported as supporting descendant context.
                upsert_frontier(
                    &mut selection.frontier_targets,
                    &request.runtime_snapshot,
                    &parent_relative_path,
                    changed_child_relative_paths,
                    Vec::new(),
                    RefineGateTargetReason::StaleDescendant,
                );
                if let Some(prior) = request.prior_gate_summaries.frontier.iter().find(|prior| {
                    prior.parent_relative_path == parent_relative_path
                        && handoff
                            .parent_node_id
                            .as_deref()
                            .or(handoff.node_id.as_deref())
                            .map_or(true, |id| id == prior.parent_node_id)
                }) {
                    let stale_reasons =
                        reasons_for_frontier(&selection.frontier_targets, &parent_relative_path);
                    selection.stale_frontier_approvals.push(StaleGateApproval {
                        target_kind: StaleGateTargetKind::Frontier,
                        node_id: prior.parent_node_id.clone(),
                        relative_path: prior.parent_relative_path.clone(),
                        gate_run_id: prior.summary.gate_run_id.clone(),
                        stale_reasons,
                    });
                }
            }
        }
    }
}

fn is_link_only_parent_text_update(
    request: &SelectRefineGateTargetsRequest,
    parent_relative_path: &str,
) -> bool {
    let text_update_count = request
        .rewrite_result
        .changed_files
        .iter()
        .filter(|file| {
            file.relative_path == parent_relative_path
                && file.change_kind == RefineChangedFileKind::TextUpdated
        })
        .count();
    let has_child_set_change = request
        .rewrite_result
        .structural_changes
        .iter()
        .any(|change| {
            change.parent_relative_path == parent_relative_path
                && change.change_kind == RefineStructuralChangeKind::ChangedChildSet
        });
    let has_parent_contract_change =
        request
            .rewrite_result
            .structural_changes
            .iter()
            .any(|change| {
                change.parent_relative_path == parent_relative_path
                    && change.change_kind == RefineStructuralChangeKind::ParentContractChanged
            });
    text_update_count == 1 && has_child_set_change && !has_parent_contract_change
}

fn upsert_descendant_leaf_targets(
    targets: &mut Vec<RefineLeafGateTarget>,
    snapshot: &RefineRuntimeNodeSnapshot,
    parent_relative_path: &str,
    explicitly_removed_paths: &[&str],
    reason: RefineGateTargetReason,
) {
    let Some(parent_node) = find_node(snapshot, parent_relative_path) else {
        return;
    };
    for child_relative_path in &parent_node.child_relative_paths {
        upsert_descendant_leaf_target(
            targets,
            snapshot,
            child_relative_path,
            explicitly_removed_paths,
            reason.clone(),
        );
    }
}

fn upsert_descendant_leaf_target(
    targets: &mut Vec<RefineLeafGateTarget>,
    snapshot: &RefineRuntimeNodeSnapshot,
    relative_path: &str,
    explicitly_removed_paths: &[&str],
    reason: RefineGateTargetReason,
) {
    if explicitly_removed_paths
        .iter()
        .any(|removed| *removed == relative_path)
    {
        return;
    }
    let Some(node) = find_node(snapshot, relative_path) else {
        return;
    };
    match node.node_kind {
        NodeKind::Leaf => upsert_leaf(
            targets,
            snapshot,
            &node.relative_path,
            Some(node.node_id.clone()),
            None,
            reason,
        ),
        NodeKind::Parent => {
            for child_relative_path in &node.child_relative_paths {
                upsert_descendant_leaf_target(
                    targets,
                    snapshot,
                    child_relative_path,
                    explicitly_removed_paths,
                    reason.clone(),
                );
            }
        }
    }
}

fn upsert_leaf(
    targets: &mut Vec<RefineLeafGateTarget>,
    snapshot: &RefineRuntimeNodeSnapshot,
    relative_path: &str,
    explicit_node_id: Option<String>,
    explicit_parent_relative_path: Option<String>,
    reason: RefineGateTargetReason,
) {
    if let Some(existing) = targets
        .iter_mut()
        .find(|target| target.relative_path == relative_path)
    {
        if explicit_parent_relative_path.is_some() {
            existing.parent_relative_path = explicit_parent_relative_path;
        }
        push_reason(&mut existing.reasons, reason);
        return;
    }
    let runtime_node = find_node(snapshot, relative_path);
    targets.push(RefineLeafGateTarget {
        node_id: explicit_node_id.or_else(|| runtime_node.map(|node| node.node_id.clone())),
        relative_path: relative_path.to_owned(),
        parent_relative_path: explicit_parent_relative_path.or_else(|| {
            runtime_node
                .and_then(|node| node.parent_relative_path.clone())
                .or_else(|| parent_self_path(snapshot, relative_path))
        }),
        reasons: vec![reason],
    });
}

fn upsert_frontier(
    targets: &mut Vec<RefineFrontierGateTarget>,
    snapshot: &RefineRuntimeNodeSnapshot,
    parent_relative_path: &str,
    changed_child_relative_paths: Vec<String>,
    removed_child_relative_paths: Vec<String>,
    reason: RefineGateTargetReason,
) {
    let deduped_children = stable_dedup_strings(changed_child_relative_paths);
    let deduped_removed_children = stable_dedup_strings(removed_child_relative_paths);
    if let Some(existing) = targets
        .iter_mut()
        .find(|target| target.parent_relative_path == parent_relative_path)
    {
        for child in deduped_children {
            if !existing.changed_child_relative_paths.contains(&child) {
                existing.changed_child_relative_paths.push(child);
            }
        }
        for child in deduped_removed_children {
            if !existing.removed_child_relative_paths.contains(&child) {
                existing.removed_child_relative_paths.push(child);
            }
        }
        push_reason(&mut existing.reasons, reason);
        return;
    }
    let runtime_node = find_node(snapshot, parent_relative_path);
    targets.push(RefineFrontierGateTarget {
        parent_node_id: runtime_node.map(|node| node.node_id.clone()),
        parent_relative_path: parent_relative_path.to_owned(),
        changed_child_relative_paths: deduped_children,
        removed_child_relative_paths: deduped_removed_children,
        reasons: vec![reason],
    });
}

fn find_node<'a>(
    snapshot: &'a RefineRuntimeNodeSnapshot,
    relative_path: &str,
) -> Option<&'a RefineRuntimeNodeSummary> {
    snapshot
        .nodes
        .iter()
        .find(|node| node.relative_path == relative_path)
}

fn reasons_for_leaf(
    targets: &[RefineLeafGateTarget],
    relative_path: &str,
) -> Vec<RefineGateTargetReason> {
    targets
        .iter()
        .find(|target| target.relative_path == relative_path)
        .map(|target| target.reasons.clone())
        .unwrap_or_else(|| vec![RefineGateTargetReason::StaleDescendant])
}

fn reasons_for_frontier(
    targets: &[RefineFrontierGateTarget],
    parent_relative_path: &str,
) -> Vec<RefineGateTargetReason> {
    targets
        .iter()
        .find(|target| target.parent_relative_path == parent_relative_path)
        .map(|target| target.reasons.clone())
        .unwrap_or_else(|| vec![RefineGateTargetReason::StaleDescendant])
}

fn push_reason(reasons: &mut Vec<RefineGateTargetReason>, reason: RefineGateTargetReason) {
    // Deduplication preserves all applicable reasons in stable order.
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn stable_dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

fn is_parent_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let parent_dir = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    stem.is_some() && parent_dir == stem
}

fn is_parent_target_path(request: &SelectRefineGateTargetsRequest, relative_path: &str) -> bool {
    find_node(&request.runtime_snapshot, relative_path)
        .is_some_and(|node| node.node_kind == NodeKind::Parent)
        || is_parent_path(relative_path)
        || request
            .rewrite_result
            .structural_changes
            .iter()
            .any(|change| change.parent_relative_path == relative_path)
}

fn known_parent_paths(selection: &RefineGateTargetSelection) -> HashSet<String> {
    let mut known = HashSet::new();
    for target in &selection.frontier_targets {
        known.insert(target.parent_relative_path.clone());
    }
    for target in &selection.leaf_targets {
        if let Some(parent_relative_path) = &target.parent_relative_path {
            known.insert(parent_relative_path.clone());
        }
    }
    known
}

fn parent_self_path(snapshot: &RefineRuntimeNodeSnapshot, relative_path: &str) -> Option<String> {
    root_scope_direct_parent_path(relative_path, 1)
        .filter(|candidate| {
            find_node(snapshot, candidate).is_some_and(|node| node.node_kind == NodeKind::Parent)
        })
        .or_else(|| top_level_root_parent_path(snapshot, relative_path))
        .or_else(|| scoped_parent_self_path(relative_path))
}

fn top_level_root_parent_path(
    snapshot: &RefineRuntimeNodeSnapshot,
    relative_path: &str,
) -> Option<String> {
    if Path::new(relative_path).components().count() != 1 {
        return None;
    }
    let root_parent_paths = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.node_kind == NodeKind::Parent
                && node.parent_node_id.is_none()
                && Path::new(&node.relative_path).components().count() == 1
                && node.relative_path != relative_path
        })
        .map(|node| node.relative_path.clone())
        .collect::<Vec<_>>();
    if root_parent_paths.len() == 1 {
        root_parent_paths.into_iter().next()
    } else {
        None
    }
}

fn scoped_parent_self_path(relative_path: &str) -> Option<String> {
    let path = Path::new(relative_path);
    let parent = path.parent()?;
    let name = parent.file_name()?.to_str()?;
    let self_path = parent
        .join(format!("{name}.md"))
        .to_string_lossy()
        .into_owned();
    (self_path != relative_path).then_some(self_path)
}

fn ancestor_parent_self_path(
    relative_path: &str,
    known_parent_paths: &HashSet<String>,
) -> Option<String> {
    root_scope_direct_parent_path(relative_path, 2)
        .filter(|candidate| known_parent_paths.contains(candidate))
        .or_else(|| scoped_ancestor_parent_self_path(relative_path))
}

fn scoped_ancestor_parent_self_path(relative_path: &str) -> Option<String> {
    let path = Path::new(relative_path);
    let parent = path.parent()?;
    let ancestor = parent.parent()?;
    let name = ancestor.file_name()?.to_str()?;
    let self_path = ancestor
        .join(format!("{name}.md"))
        .to_string_lossy()
        .into_owned();
    (self_path != relative_path).then_some(self_path)
}

fn root_scope_direct_parent_path(
    relative_path: &str,
    child_component_count: usize,
) -> Option<String> {
    let components = Path::new(relative_path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(component) => component.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.len() != child_component_count + 1 {
        return None;
    }
    let parent_relative_path = format!("{}.md", components[0]);
    (parent_relative_path != relative_path).then_some(parent_relative_path)
}

#[allow(dead_code)]
fn _type_shape_anchors(_node_kind: NodeKind, _summary: GateSummary) {}
