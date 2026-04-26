use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::decision::{RefineDecision, RefineDecisionStatus, RefineRewriteActionKind};

/// Rewrite Scope Planning: rewrite scope planning scaffold.
///
/// Ordered slots for sibling behavior:
/// 1. target selection
/// 2. stale and preserved path planning
/// 3. link maintenance planning
/// 4. node creation and removal planning
/// 5. rewrite planning conflict detection
/// 6. rewrite scope handoff summary
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineRewriteScope {
    pub rewrite_targets: Vec<RefineRewriteTarget>,
    pub preserved_paths: Vec<String>,
    pub stale_descendants: Vec<RefineStaleDescendant>,
    pub link_changes: Vec<RefineLinkChange>,
    pub node_creations: Vec<RefineNodeCreation>,
    pub node_removals: Vec<RefineNodeRemoval>,
    pub conflicts: Vec<RefineRewriteScopeConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRewriteTarget {
    pub relative_path: String,
    pub node_id: Option<String>,
    pub action_kind: RefineRewriteActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineStaleDescendant {
    pub relative_path: String,
    pub node_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineLinkChange {
    pub parent_relative_path: String,
    pub add_child_relative_paths: Vec<String>,
    pub remove_child_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineNodeCreation {
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub node_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineNodeRemoval {
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub node_id: Option<String>,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineNodeCreationRemovalPlan {
    pub node_creations: Vec<RefineNodeCreation>,
    pub node_removals: Vec<RefineNodeRemoval>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRewriteScopeConflict {
    pub relative_path: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRewriteScopeRequest {
    pub plan_id: String,
    pub plan_root: PathBuf,
    pub decisions: Vec<RefineDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefineRewriteScopeError {
    MissingPlanRoot,
    UnconfirmedDecision,
    UnmappedImpact,
    ConflictingRewriteTargets,
    MissingLinkedNode,
    InvalidScope,
}

impl fmt::Display for RefineRewriteScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RefineRewriteScopeError {}

pub fn plan_refine_rewrite_scope(
    request: RefineRewriteScopeRequest,
) -> Result<RefineRewriteScope, RefineRewriteScopeError> {
    if !request.plan_root.is_dir() {
        return Err(RefineRewriteScopeError::MissingPlanRoot);
    }

    let mut scope = RefineRewriteScope::default();
    for decision in request.decisions {
        if matches!(
            decision.confirmation.status,
            RefineDecisionStatus::AwaitingManualConfirmation
                | RefineDecisionStatus::UserDecisionBlocked
        ) {
            return Err(RefineRewriteScopeError::UnconfirmedDecision);
        }

        for action in decision.rewrite_actions {
            match action.action_kind {
                RefineRewriteActionKind::UpdateExistingNode
                | RefineRewriteActionKind::MarkStale => {
                    let Some(relative_path) = action.target_relative_path else {
                        return Err(RefineRewriteScopeError::UnmappedImpact);
                    };
                    scope.rewrite_targets.push(RefineRewriteTarget {
                        relative_path,
                        node_id: None,
                        action_kind: action.action_kind,
                    });
                }
                RefineRewriteActionKind::CreateNode => {
                    let Some(relative_path) = action.target_relative_path else {
                        return Err(RefineRewriteScopeError::UnmappedImpact);
                    };
                    scope.node_creations.push(RefineNodeCreation {
                        relative_path,
                        parent_relative_path: action.parent_relative_path,
                        node_kind: action.node_kind,
                    });
                }
                RefineRewriteActionKind::RemoveNode => {
                    let Some(relative_path) = action.target_relative_path else {
                        return Err(RefineRewriteScopeError::UnmappedImpact);
                    };
                    scope.node_removals.push(RefineNodeRemoval {
                        relative_path,
                        parent_relative_path: action.parent_relative_path,
                        node_id: None,
                        explicit: true,
                    });
                }
                RefineRewriteActionKind::UpdateLinks => {
                    let Some(parent_relative_path) = action.parent_relative_path else {
                        return Err(RefineRewriteScopeError::MissingLinkedNode);
                    };
                    let mut link_change = RefineLinkChange {
                        parent_relative_path,
                        add_child_relative_paths: Vec::new(),
                        remove_child_relative_paths: Vec::new(),
                    };
                    if let Some(target) = action.target_relative_path {
                        link_change.add_child_relative_paths.push(target);
                    }
                    scope.link_changes.push(link_change);
                }
            }
        }
    }

    Ok(scope)
}
