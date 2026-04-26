use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::{EnsureNodeIdRequest, InspectNodeRequest, ListChildrenRequest, NodeKind, Runtime};

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
    // Uses only public Runtime::ensure_node_id, Runtime::inspect_node, and Runtime::list_children;
    // must not edit crates/gen-plan/core/src/runtime/mod.rs or private runtime query modules.
    let mut registered = RegisteredRefineGateTargets::default();
    let mut tracked_parents = HashSet::<String>::new();

    for candidate in ordered_parent_candidates(request.parent_candidates) {
        validate_non_empty_reasons("parent", &candidate.relative_path, &candidate.reasons)?;
        validate_parent_path("relative_path", &candidate.relative_path)?;
        if let Some(parent_relative_path) = &candidate.parent_relative_path {
            validate_parent_path("parent_relative_path", parent_relative_path)?;
            if !tracked_parents.contains(parent_relative_path)
                && inspect_parent(runtime, &request.plan_id, parent_relative_path).is_err()
            {
                return Err(RefineGatePreparationError::MissingParentRegistration {
                    child_relative_path: candidate.relative_path.clone(),
                    parent_relative_path: parent_relative_path.clone(),
                });
            }
        }
        let response = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: request.plan_id.clone(),
                relative_path: candidate.relative_path.clone(),
                parent_relative_path: candidate.parent_relative_path.clone(),
            })
            .map_err(|source| RefineGatePreparationError::RegistrationFailed {
                relative_path: candidate.relative_path.clone(),
                source: source.to_string(),
            })?;
        tracked_parents.insert(candidate.relative_path.clone());
        registered
            .parent_targets
            .push(RegisteredRefineParentTarget {
                node_id: response.node_id,
                relative_path: candidate.relative_path,
                parent_relative_path: candidate.parent_relative_path,
                reasons: stable_dedup_reasons(candidate.reasons),
            });
    }

    for candidate in request.leaf_candidates {
        validate_non_empty_reasons("leaf", &candidate.relative_path, &candidate.reasons)?;
        validate_leaf_path("relative_path", &candidate.relative_path)?;
        validate_leaf_parent(
            runtime,
            &request.plan_id,
            &candidate.relative_path,
            candidate.parent_relative_path.as_deref(),
            &tracked_parents,
        )?;
        let response = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: request.plan_id.clone(),
                relative_path: candidate.relative_path.clone(),
                parent_relative_path: candidate.parent_relative_path.clone(),
            })
            .map_err(|source| RefineGatePreparationError::RegistrationFailed {
                relative_path: candidate.relative_path.clone(),
                source: source.to_string(),
            })?;
        registered.leaf_targets.push(RegisteredRefineLeafTarget {
            node_id: response.node_id,
            relative_path: candidate.relative_path,
            parent_relative_path: candidate.parent_relative_path,
            reasons: stable_dedup_reasons(candidate.reasons),
        });
    }

    for candidate in request.frontier_candidates {
        validate_non_empty_reasons(
            "frontier",
            &candidate.parent_relative_path,
            &candidate.reasons,
        )?;
        validate_parent_path("parent_relative_path", &candidate.parent_relative_path)?;
        let parent = inspect_parent(runtime, &request.plan_id, &candidate.parent_relative_path)
            .map_err(|_| RefineGatePreparationError::MissingParentRegistration {
                child_relative_path: candidate.parent_relative_path.clone(),
                parent_relative_path: candidate.parent_relative_path.clone(),
            })?;
        let children = runtime
            .list_children(ListChildrenRequest {
                plan_id: request.plan_id.clone(),
                parent_node_id: Some(parent.node_id.clone()),
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
        let missing: Vec<String> = candidate
            .changed_child_relative_paths
            .iter()
            .filter(|child| !child_paths.contains(child.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(RefineGatePreparationError::IncoherentFrontierChildren {
                parent_relative_path: candidate.parent_relative_path,
                missing_child_relative_paths: missing,
            });
        }
        registered
            .frontier_targets
            .push(RegisteredRefineFrontierTarget {
                parent_node_id: parent.node_id,
                parent_relative_path: candidate.parent_relative_path,
                changed_child_relative_paths: stable_dedup_strings(
                    candidate.changed_child_relative_paths,
                ),
                reasons: stable_dedup_reasons(candidate.reasons),
            });
    }

    Ok(registered)
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
            validate_parent_path("parent_relative_path", parent)?;
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

fn validate_parent_path(
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
    if stem.is_none() || parent_dir != stem {
        return Err(RefineGatePreparationError::InvalidCanonicalPath {
            field: field.to_owned(),
            relative_path: relative_path.to_owned(),
        });
    }
    Ok(())
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
