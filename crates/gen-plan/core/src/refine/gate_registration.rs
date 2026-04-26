use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::{
    EnsureNodeIdRequest, InspectNodeRequest, ListChildrenRequest, NodeKind, OpenPlanRequest,
    ReconcileParentChildLinksRequest, Runtime,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRefineGateTargetsRequest {
    pub plan_id: String,
    pub parent_candidates: Vec<RefineParentRegistrationCandidate>,
    pub leaf_candidates: Vec<RefineLeafRegistrationCandidate>,
    pub frontier_candidates: Vec<RefineFrontierRegistrationCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineParentRegistrationCandidate {
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineLeafRegistrationCandidate {
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineFrontierRegistrationCandidate {
    pub parent_relative_path: String,
    pub changed_child_relative_paths: Vec<String>,
    #[serde(default)]
    pub removed_child_relative_paths: Vec<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRefineParentTarget {
    pub node_id: String,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRefineLeafTarget {
    pub node_id: String,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRefineFrontierTarget {
    pub parent_node_id: String,
    pub parent_relative_path: String,
    pub changed_child_relative_paths: Vec<String>,
    pub reasons: Vec<RefineGateTargetReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RegisteredRefineGateTargets {
    pub parent_targets: Vec<RegisteredRefineParentTarget>,
    pub leaf_targets: Vec<RegisteredRefineLeafTarget>,
    pub frontier_targets: Vec<RegisteredRefineFrontierTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineGateTargetReason {
    TextChanged,
    ContextInvalidated,
    NewLeaf,
    NewParent,
    RegeneratedLeaf,
    ChangedChildSet,
    ParentContractChanged,
    StaleDescendant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefineGatePreparationError {
    MissingParentRegistration {
        child_relative_path: String,
        parent_relative_path: String,
    },
    InvalidCanonicalPath {
        field: String,
        relative_path: String,
    },
    EmptySelectionReasons {
        candidate_kind: String,
        relative_path: String,
    },
    IncoherentFrontierChildren {
        parent_relative_path: String,
        missing_child_relative_paths: Vec<String>,
    },
    RegistrationFailed {
        relative_path: String,
        source: String,
    },
}

impl fmt::Display for RefineGatePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RefineGatePreparationError {}

pub fn register_refine_gate_targets(
    runtime: &Runtime,
    request: RegisterRefineGateTargetsRequest,
) -> Result<RegisteredRefineGateTargets, RefineGatePreparationError> {
    // Validate the whole request before any ensure/reconcile calls so rejected refine targets do
    // not leave partially registered runtime nodes behind.
    let RegisterRefineGateTargetsRequest {
        plan_id,
        parent_candidates,
        leaf_candidates,
        frontier_candidates,
    } = request;
    let parent_candidates = ordered_parent_candidates(parent_candidates);
    prevalidate_registration_request(
        runtime,
        &plan_id,
        &parent_candidates,
        &leaf_candidates,
        &frontier_candidates,
    )?;
    let mut registered = RegisteredRefineGateTargets::default();
    let mut tracked_parents = HashSet::<String>::new();

    for candidate in parent_candidates {
        validate_non_empty_reasons("parent", &candidate.relative_path, &candidate.reasons)?;
        validate_parent_path(runtime, &plan_id, "relative_path", &candidate.relative_path)?;
        if let Some(parent_relative_path) = &candidate.parent_relative_path {
            validate_parent_path(
                runtime,
                &plan_id,
                "parent_relative_path",
                parent_relative_path,
            )?;
            if !tracked_parents.contains(parent_relative_path)
                && inspect_parent(runtime, &plan_id, parent_relative_path).is_err()
            {
                return Err(RefineGatePreparationError::MissingParentRegistration {
                    child_relative_path: candidate.relative_path.clone(),
                    parent_relative_path: parent_relative_path.clone(),
                });
            }
        }
        let node_id =
            if let Ok(existing) = inspect_parent(runtime, &plan_id, &candidate.relative_path) {
                if existing.parent_relative_path != candidate.parent_relative_path {
                    return Err(RefineGatePreparationError::RegistrationFailed {
                        relative_path: candidate.relative_path.clone(),
                        source: "existing parent target has conflicting parent linkage".to_owned(),
                    });
                }
                existing.node_id
            } else {
                runtime
                    .ensure_node_id(EnsureNodeIdRequest {
                        plan_id: plan_id.clone(),
                        relative_path: candidate.relative_path.clone(),
                        parent_relative_path: candidate.parent_relative_path.clone(),
                    })
                    .map_err(|source| RefineGatePreparationError::RegistrationFailed {
                        relative_path: candidate.relative_path.clone(),
                        source: source.to_string(),
                    })?
                    .node_id
            };
        tracked_parents.insert(candidate.relative_path.clone());
        registered
            .parent_targets
            .push(RegisteredRefineParentTarget {
                node_id,
                relative_path: candidate.relative_path,
                parent_relative_path: candidate.parent_relative_path,
                reasons: stable_dedup_reasons(candidate.reasons),
            });
    }

    for candidate in &frontier_candidates {
        validate_frontier_candidate(runtime, &plan_id, candidate)?;
    }

    let mut deferred_leaf_candidates = Vec::new();
    for candidate in leaf_candidates {
        validate_non_empty_reasons("leaf", &candidate.relative_path, &candidate.reasons)?;
        validate_leaf_path("relative_path", &candidate.relative_path)?;
        validate_leaf_parent(
            runtime,
            &plan_id,
            &candidate.relative_path,
            candidate.parent_relative_path.as_deref(),
            &tracked_parents,
        )?;
        if leaf_needs_child_link_reconciliation(runtime, &plan_id, &candidate) {
            deferred_leaf_candidates.push(candidate);
        } else {
            registered
                .leaf_targets
                .push(register_leaf_candidate(runtime, &plan_id, candidate)?);
        }
    }

    let mut reconciled_frontiers = HashMap::<String, HashSet<String>>::new();
    let mut frontier_parent_node_ids = HashMap::<String, String>::new();
    for candidate in &frontier_candidates {
        let parent =
            inspect_parent(runtime, &plan_id, &candidate.parent_relative_path).map_err(|_| {
                RefineGatePreparationError::MissingParentRegistration {
                    child_relative_path: candidate.parent_relative_path.clone(),
                    parent_relative_path: candidate.parent_relative_path.clone(),
                }
            })?;
        frontier_parent_node_ids.insert(candidate.parent_relative_path.clone(), parent.node_id);
    }

    for candidate in &frontier_candidates {
        validate_frontier_changed_child_paths(runtime, &plan_id, candidate)?;
    }

    for candidate in &frontier_candidates {
        if !frontier_needs_child_link_reconciliation(candidate)
            || reconciled_frontiers.contains_key(&candidate.parent_relative_path)
        {
            continue;
        }
        let reconciled = runtime
            .reconcile_parent_child_links(ReconcileParentChildLinksRequest {
                plan_id: plan_id.clone(),
                parent_relative_path: candidate.parent_relative_path.clone(),
            })
            .map_err(|source| RefineGatePreparationError::RegistrationFailed {
                relative_path: candidate.parent_relative_path.clone(),
                source: source.to_string(),
            })?;
        reconciled_frontiers.insert(
            candidate.parent_relative_path.clone(),
            reconciled
                .linked_child_relative_paths
                .into_iter()
                .collect::<HashSet<_>>(),
        );
    }

    for candidate in deferred_leaf_candidates {
        registered
            .leaf_targets
            .push(register_leaf_candidate(runtime, &plan_id, candidate)?);
    }

    for candidate in frontier_candidates {
        let parent_node_id = frontier_parent_node_ids
            .get(&candidate.parent_relative_path)
            .cloned()
            .ok_or_else(|| RefineGatePreparationError::MissingParentRegistration {
                child_relative_path: candidate.parent_relative_path.clone(),
                parent_relative_path: candidate.parent_relative_path.clone(),
            })?;
        let children = runtime
            .list_children(ListChildrenRequest {
                plan_id: plan_id.clone(),
                parent_node_id: Some(parent_node_id.clone()),
                parent_relative_path: None,
            })
            .map_err(|source| RefineGatePreparationError::RegistrationFailed {
                relative_path: candidate.parent_relative_path.clone(),
                source: source.to_string(),
            })?;
        let child_paths: HashSet<_> = children
            .children
            .iter()
            .map(|child| child.relative_path.as_str())
            .collect();
        let linked_child_paths = reconciled_frontiers
            .get(&candidate.parent_relative_path)
            .cloned()
            .unwrap_or_default();
        let missing: Vec<String> = candidate
            .changed_child_relative_paths
            .iter()
            .filter(|child| {
                linked_child_paths.contains(*child) && !child_paths.contains(child.as_str())
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(RefineGatePreparationError::IncoherentFrontierChildren {
                parent_relative_path: candidate.parent_relative_path,
                missing_child_relative_paths: missing,
            });
        }
        let changed_child_relative_paths =
            dispatchable_changed_child_relative_paths(runtime, &plan_id, &candidate);
        registered
            .frontier_targets
            .push(RegisteredRefineFrontierTarget {
                parent_node_id,
                parent_relative_path: candidate.parent_relative_path,
                changed_child_relative_paths: stable_dedup_strings(changed_child_relative_paths),
                reasons: stable_dedup_reasons(candidate.reasons),
            });
    }

    Ok(registered)
}

fn validate_frontier_candidate(
    runtime: &Runtime,
    plan_id: &str,
    candidate: &RefineFrontierRegistrationCandidate,
) -> Result<(), RefineGatePreparationError> {
    validate_non_empty_reasons(
        "frontier",
        &candidate.parent_relative_path,
        &candidate.reasons,
    )?;
    validate_parent_path(
        runtime,
        plan_id,
        "parent_relative_path",
        &candidate.parent_relative_path,
    )
}

fn prevalidate_registration_request(
    runtime: &Runtime,
    plan_id: &str,
    parent_candidates: &[RefineParentRegistrationCandidate],
    leaf_candidates: &[RefineLeafRegistrationCandidate],
    frontier_candidates: &[RefineFrontierRegistrationCandidate],
) -> Result<(), RefineGatePreparationError> {
    let candidate_parent_paths = parent_candidates
        .iter()
        .map(|candidate| candidate.relative_path.clone())
        .collect::<HashSet<_>>();
    let mut candidate_node_parent_paths = HashMap::new();
    for candidate in parent_candidates {
        candidate_node_parent_paths.insert(
            candidate.relative_path.clone(),
            candidate.parent_relative_path.clone(),
        );
    }
    for candidate in leaf_candidates {
        candidate_node_parent_paths.insert(
            candidate.relative_path.clone(),
            candidate.parent_relative_path.clone(),
        );
    }

    for candidate in parent_candidates {
        validate_non_empty_reasons("parent", &candidate.relative_path, &candidate.reasons)?;
        validate_parent_path(runtime, plan_id, "relative_path", &candidate.relative_path)?;
        prevalidate_existing_candidate_node(
            runtime,
            plan_id,
            &candidate.relative_path,
            NodeKind::Parent,
            candidate.parent_relative_path.as_deref(),
        )?;
        if let Some(parent_relative_path) = &candidate.parent_relative_path {
            validate_parent_path(
                runtime,
                plan_id,
                "parent_relative_path",
                parent_relative_path,
            )?;
            if !candidate_parent_paths.contains(parent_relative_path)
                && inspect_parent(runtime, plan_id, parent_relative_path).is_err()
            {
                return Err(RefineGatePreparationError::MissingParentRegistration {
                    child_relative_path: candidate.relative_path.clone(),
                    parent_relative_path: parent_relative_path.clone(),
                });
            }
        }
    }

    for candidate in leaf_candidates {
        validate_non_empty_reasons("leaf", &candidate.relative_path, &candidate.reasons)?;
        validate_leaf_path("relative_path", &candidate.relative_path)?;
        validate_leaf_parent_candidate(
            runtime,
            plan_id,
            &candidate.relative_path,
            candidate.parent_relative_path.as_deref(),
            &candidate_parent_paths,
        )?;
        prevalidate_existing_candidate_node(
            runtime,
            plan_id,
            &candidate.relative_path,
            NodeKind::Leaf,
            candidate.parent_relative_path.as_deref(),
        )?;
    }

    for candidate in frontier_candidates {
        validate_frontier_candidate(runtime, plan_id, candidate)?;
        if !candidate_parent_paths.contains(&candidate.parent_relative_path)
            && inspect_parent(runtime, plan_id, &candidate.parent_relative_path).is_err()
        {
            return Err(RefineGatePreparationError::MissingParentRegistration {
                child_relative_path: candidate.parent_relative_path.clone(),
                parent_relative_path: candidate.parent_relative_path.clone(),
            });
        }
        validate_frontier_changed_child_paths_before_mutation(
            runtime,
            plan_id,
            candidate,
            &candidate_node_parent_paths,
        )?;
    }

    Ok(())
}

fn register_leaf_candidate(
    runtime: &Runtime,
    plan_id: &str,
    candidate: RefineLeafRegistrationCandidate,
) -> Result<RegisteredRefineLeafTarget, RefineGatePreparationError> {
    if let Ok(existing) = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan_id.to_owned(),
        node_id: None,
        relative_path: Some(candidate.relative_path.clone()),
    }) {
        if existing.node_kind != NodeKind::Leaf {
            return Err(RefineGatePreparationError::RegistrationFailed {
                relative_path: candidate.relative_path.clone(),
                source: "existing target path is not a leaf node".to_owned(),
            });
        }
        if existing.parent_relative_path == candidate.parent_relative_path {
            return Ok(RegisteredRefineLeafTarget {
                node_id: existing.node_id,
                relative_path: candidate.relative_path,
                parent_relative_path: candidate.parent_relative_path,
                reasons: stable_dedup_reasons(candidate.reasons),
            });
        }
    }
    let response = runtime
        .ensure_node_id(EnsureNodeIdRequest {
            plan_id: plan_id.to_owned(),
            relative_path: candidate.relative_path.clone(),
            parent_relative_path: candidate.parent_relative_path.clone(),
        })
        .map_err(|source| RefineGatePreparationError::RegistrationFailed {
            relative_path: candidate.relative_path.clone(),
            source: source.to_string(),
        })?;
    Ok(RegisteredRefineLeafTarget {
        node_id: response.node_id,
        relative_path: candidate.relative_path,
        parent_relative_path: candidate.parent_relative_path,
        reasons: stable_dedup_reasons(candidate.reasons),
    })
}

fn leaf_needs_child_link_reconciliation(
    runtime: &Runtime,
    plan_id: &str,
    candidate: &RefineLeafRegistrationCandidate,
) -> bool {
    let Some(parent_relative_path) = candidate.parent_relative_path.as_deref() else {
        return false;
    };
    let Ok(node) = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan_id.to_owned(),
        node_id: None,
        relative_path: Some(candidate.relative_path.clone()),
    }) else {
        return false;
    };
    node.node_kind == NodeKind::Leaf
        && node.parent_relative_path.as_deref() != Some(parent_relative_path)
}

fn frontier_needs_child_link_reconciliation(
    candidate: &RefineFrontierRegistrationCandidate,
) -> bool {
    !candidate.changed_child_relative_paths.is_empty()
        || !candidate.removed_child_relative_paths.is_empty()
        || candidate
            .reasons
            .contains(&RefineGateTargetReason::ChangedChildSet)
}

fn validate_frontier_changed_child_paths(
    runtime: &Runtime,
    plan_id: &str,
    candidate: &RefineFrontierRegistrationCandidate,
) -> Result<(), RefineGatePreparationError> {
    for child in &candidate.removed_child_relative_paths {
        validate_markdown_path("removed_child_relative_paths", child)?;
    }
    let removed_child_paths = candidate
        .removed_child_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut missing_child_relative_paths = Vec::new();
    let linked_child_paths =
        linked_child_paths_for_parent(runtime, plan_id, &candidate.parent_relative_path)?;
    for child in &candidate.changed_child_relative_paths {
        validate_markdown_path("changed_child_relative_paths", child)?;
        if !frontier_child_path_is_in_scope(
            runtime,
            plan_id,
            candidate,
            child,
            &removed_child_paths,
            &linked_child_paths,
            None,
        ) {
            missing_child_relative_paths.push(child.clone());
        }
    }
    if missing_child_relative_paths.is_empty() {
        Ok(())
    } else {
        Err(RefineGatePreparationError::IncoherentFrontierChildren {
            parent_relative_path: candidate.parent_relative_path.clone(),
            missing_child_relative_paths,
        })
    }
}

fn dispatchable_changed_child_relative_paths(
    runtime: &Runtime,
    plan_id: &str,
    candidate: &RefineFrontierRegistrationCandidate,
) -> Vec<String> {
    let removed_child_paths = candidate
        .removed_child_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    candidate
        .changed_child_relative_paths
        .iter()
        .chain(candidate.removed_child_relative_paths.iter())
        .filter(|child| {
            if !removed_child_paths.contains(child.as_str()) {
                return true;
            }
            runtime
                .inspect_node(InspectNodeRequest {
                    plan_id: plan_id.to_owned(),
                    node_id: None,
                    relative_path: Some((*child).clone()),
                })
                .is_ok()
        })
        .cloned()
        .collect()
}

fn ordered_parent_candidates(
    mut candidates: Vec<RefineParentRegistrationCandidate>,
) -> Vec<RefineParentRegistrationCandidate> {
    // topologically order parent_candidates by canonical ancestry.
    // Out-of-order nested parent candidates are valid input.
    candidates.sort_by_key(|candidate| path_depth(&candidate.relative_path));
    candidates
}

fn validate_leaf_parent(
    runtime: &Runtime,
    plan_id: &str,
    relative_path: &str,
    parent_relative_path: Option<&str>,
    tracked_parents: &HashSet<String>,
) -> Result<(), RefineGatePreparationError> {
    match parent_relative_path {
        None if !relative_path.contains('/') => Ok(()), // root-level leaf path
        None => Err(RefineGatePreparationError::MissingParentRegistration {
            child_relative_path: relative_path.to_owned(),
            parent_relative_path: parent_self_path(relative_path).unwrap_or_default(),
        }),
        Some(parent) => {
            validate_parent_path(runtime, plan_id, "parent_relative_path", parent)?;
            if tracked_parents.contains(parent) || inspect_parent(runtime, plan_id, parent).is_ok()
            {
                Ok(())
            } else {
                Err(RefineGatePreparationError::MissingParentRegistration {
                    child_relative_path: relative_path.to_owned(),
                    parent_relative_path: parent.to_owned(),
                })
            }
        }
    }
}

fn validate_leaf_parent_candidate(
    runtime: &Runtime,
    plan_id: &str,
    relative_path: &str,
    parent_relative_path: Option<&str>,
    candidate_parent_paths: &HashSet<String>,
) -> Result<(), RefineGatePreparationError> {
    match parent_relative_path {
        None if !relative_path.contains('/') => Ok(()),
        None => Err(RefineGatePreparationError::MissingParentRegistration {
            child_relative_path: relative_path.to_owned(),
            parent_relative_path: parent_self_path(relative_path).unwrap_or_default(),
        }),
        Some(parent) => {
            validate_parent_path(runtime, plan_id, "parent_relative_path", parent)?;
            if candidate_parent_paths.contains(parent)
                || inspect_parent(runtime, plan_id, parent).is_ok()
            {
                Ok(())
            } else {
                Err(RefineGatePreparationError::MissingParentRegistration {
                    child_relative_path: relative_path.to_owned(),
                    parent_relative_path: parent.to_owned(),
                })
            }
        }
    }
}

fn inspect_parent(
    runtime: &Runtime,
    plan_id: &str,
    parent_relative_path: &str,
) -> Result<crate::InspectNodeResponse, anyhow::Error> {
    let node = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan_id.to_owned(),
        node_id: None,
        relative_path: Some(parent_relative_path.to_owned()),
    })?;
    if node.node_kind != NodeKind::Parent {
        anyhow::bail!("tracked node is not a parent")
    }
    Ok(node)
}

fn validate_non_empty_reasons(
    candidate_kind: &str,
    relative_path: &str,
    reasons: &[RefineGateTargetReason],
) -> Result<(), RefineGatePreparationError> {
    if reasons.is_empty() {
        return Err(RefineGatePreparationError::EmptySelectionReasons {
            candidate_kind: candidate_kind.to_owned(),
            relative_path: relative_path.to_owned(),
        });
    }
    Ok(())
}

fn validate_leaf_path(field: &str, relative_path: &str) -> Result<(), RefineGatePreparationError> {
    validate_markdown_path(field, relative_path)?;
    let path = Path::new(relative_path);
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let parent_dir = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    if stem.is_some() && parent_dir == stem {
        return Err(RefineGatePreparationError::InvalidCanonicalPath {
            field: field.to_owned(),
            relative_path: relative_path.to_owned(),
        });
    }
    Ok(())
}

fn validate_frontier_changed_child_paths_before_mutation(
    runtime: &Runtime,
    plan_id: &str,
    candidate: &RefineFrontierRegistrationCandidate,
    candidate_node_parent_paths: &HashMap<String, Option<String>>,
) -> Result<(), RefineGatePreparationError> {
    for child in &candidate.removed_child_relative_paths {
        validate_markdown_path("removed_child_relative_paths", child)?;
    }
    let removed_child_paths = candidate
        .removed_child_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut missing_child_relative_paths = Vec::new();
    let linked_child_paths =
        linked_child_paths_for_parent(runtime, plan_id, &candidate.parent_relative_path)?;
    for child in &candidate.changed_child_relative_paths {
        validate_markdown_path("changed_child_relative_paths", child)?;
        if !frontier_child_path_is_in_scope(
            runtime,
            plan_id,
            candidate,
            child,
            &removed_child_paths,
            &linked_child_paths,
            Some(candidate_node_parent_paths),
        ) {
            missing_child_relative_paths.push(child.clone());
        }
    }
    if missing_child_relative_paths.is_empty() {
        Ok(())
    } else {
        Err(RefineGatePreparationError::IncoherentFrontierChildren {
            parent_relative_path: candidate.parent_relative_path.clone(),
            missing_child_relative_paths,
        })
    }
}

fn prevalidate_existing_candidate_node(
    runtime: &Runtime,
    plan_id: &str,
    relative_path: &str,
    expected_kind: NodeKind,
    expected_parent_relative_path: Option<&str>,
) -> Result<(), RefineGatePreparationError> {
    let Ok(existing) = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan_id.to_owned(),
        node_id: None,
        relative_path: Some(relative_path.to_owned()),
    }) else {
        return Ok(());
    };
    if existing.node_kind != expected_kind {
        return Err(RefineGatePreparationError::RegistrationFailed {
            relative_path: relative_path.to_owned(),
            source: format!(
                "existing node is `{}`, but registration requested `{}`",
                existing.node_kind.as_str(),
                expected_kind.as_str()
            ),
        });
    }
    if existing.parent_relative_path.as_deref() == expected_parent_relative_path {
        return Ok(());
    }
    if let Some(parent_relative_path) = expected_parent_relative_path {
        let linked_child_paths =
            linked_child_paths_for_parent(runtime, plan_id, parent_relative_path)?;
        if linked_child_paths.contains(relative_path) {
            return Ok(());
        }
    }
    Err(RefineGatePreparationError::RegistrationFailed {
        relative_path: relative_path.to_owned(),
        source: format!(
            "existing node parent_relative_path {:?} conflicts with requested parent_relative_path {:?}",
            existing.parent_relative_path, expected_parent_relative_path
        ),
    })
}

fn frontier_child_path_is_in_scope(
    runtime: &Runtime,
    plan_id: &str,
    candidate: &RefineFrontierRegistrationCandidate,
    child: &str,
    removed_child_paths: &HashSet<&str>,
    linked_child_paths: &HashSet<String>,
    candidate_node_parent_paths: Option<&HashMap<String, Option<String>>>,
) -> bool {
    if candidate_node_parent_paths.is_some_and(|candidate_paths| {
        candidate_paths.get(child).is_some_and(|parent| {
            parent.as_deref() == Some(candidate.parent_relative_path.as_str())
        })
    }) {
        return true;
    }
    if linked_child_paths.contains(child) {
        return true;
    }
    match runtime.inspect_node(InspectNodeRequest {
        plan_id: plan_id.to_owned(),
        node_id: None,
        relative_path: Some(child.to_owned()),
    }) {
        Ok(node) => {
            node.parent_relative_path.as_deref() == Some(candidate.parent_relative_path.as_str())
                || (removed_child_paths.contains(child) && node.parent_relative_path.is_none())
        }
        Err(_) => removed_child_paths.contains(child),
    }
}

fn linked_child_paths_for_parent(
    runtime: &Runtime,
    plan_id: &str,
    parent_relative_path: &str,
) -> Result<HashSet<String>, RefineGatePreparationError> {
    let plan_root = runtime.persisted_plan_root(plan_id).map_err(|source| {
        RefineGatePreparationError::RegistrationFailed {
            relative_path: parent_relative_path.to_owned(),
            source: source.to_string(),
        }
    })?;
    let markdown = fs::read_to_string(plan_root.join(parent_relative_path)).map_err(|source| {
        RefineGatePreparationError::RegistrationFailed {
            relative_path: parent_relative_path.to_owned(),
            source: source.to_string(),
        }
    })?;
    crate::runtime::child_links::parse_child_node_link_paths(parent_relative_path, &markdown)
        .map(|paths| paths.into_iter().collect())
        .map_err(|source| RefineGatePreparationError::RegistrationFailed {
            relative_path: parent_relative_path.to_owned(),
            source: source.to_string(),
        })
}

fn validate_parent_path(
    runtime: &Runtime,
    plan_id: &str,
    field: &str,
    relative_path: &str,
) -> Result<(), RefineGatePreparationError> {
    validate_markdown_path(field, relative_path)?;
    let path = Path::new(relative_path);
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let parent_dir = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    let is_scoped_parent_path = stem.is_some() && parent_dir == stem;
    let is_root_plan_parent_path = stem.is_some()
        && is_root_parent_path(relative_path)
        && is_actual_root_plan_parent_path(runtime, plan_id, relative_path);
    if !is_scoped_parent_path && !is_root_plan_parent_path {
        return Err(RefineGatePreparationError::InvalidCanonicalPath {
            field: field.to_owned(),
            relative_path: relative_path.to_owned(),
        });
    }
    Ok(())
}

fn is_actual_root_plan_parent_path(runtime: &Runtime, plan_id: &str, relative_path: &str) -> bool {
    let Some(plan_name) = Path::new(relative_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
    else {
        return false;
    };
    runtime
        .open_plan(OpenPlanRequest {
            plan_name: plan_name.to_owned(),
        })
        .map(|plan| plan.plan_id == plan_id)
        .unwrap_or(false)
}

fn is_root_parent_path(relative_path: &str) -> bool {
    Path::new(relative_path).components().count() == 1
}

fn validate_markdown_path(
    field: &str,
    relative_path: &str,
) -> Result<(), RefineGatePreparationError> {
    let path = Path::new(relative_path);
    if relative_path.is_empty()
        || path.is_absolute()
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return Err(RefineGatePreparationError::InvalidCanonicalPath {
            field: field.to_owned(),
            relative_path: relative_path.to_owned(),
        });
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(RefineGatePreparationError::InvalidCanonicalPath {
                field: field.to_owned(),
                relative_path: relative_path.to_owned(),
            });
        }
    }
    Ok(())
}

fn parent_self_path(relative_path: &str) -> Option<String> {
    let parent = Path::new(relative_path).parent()?;
    let name = parent.file_name()?.to_str()?;
    Some(
        parent
            .join(format!("{name}.md"))
            .to_string_lossy()
            .into_owned(),
    )
}

fn path_depth(relative_path: &str) -> usize {
    Path::new(relative_path).components().count()
}

fn stable_dedup_reasons(reasons: Vec<RefineGateTargetReason>) -> Vec<RefineGateTargetReason> {
    let mut result = Vec::new();
    for reason in reasons {
        if !result.contains(&reason) {
            result.push(reason);
        }
    }
    result
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
