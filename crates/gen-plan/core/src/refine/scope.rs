use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::decision::{
    RefineDecision, RefineDecisionStatus, RefineRewriteActionKind, RefineRewriteLinkChangeKind,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewritePathIntent {
    Update,
    MarkStale,
    Create,
    Remove,
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
    let mut path_intents = HashMap::<String, RewritePathIntent>::new();
    for decision in request.decisions {
        if matches!(
            decision.confirmation.status,
            RefineDecisionStatus::AwaitingManualConfirmation
                | RefineDecisionStatus::UserDecisionBlocked
        ) {
            return Err(RefineRewriteScopeError::UnconfirmedDecision);
        }

        let decision_rationale = decision.confirmation.rationale.clone();
        for action in decision.rewrite_actions {
            match action.action_kind {
                RefineRewriteActionKind::UpdateExistingNode => {
                    let Some(relative_path) = action.target_relative_path else {
                        return Err(RefineRewriteScopeError::UnmappedImpact);
                    };
                    record_path_intent(
                        &mut path_intents,
                        &relative_path,
                        RewritePathIntent::Update,
                    )?;
                    scope.rewrite_targets.push(RefineRewriteTarget {
                        relative_path,
                        node_id: None,
                        action_kind: action.action_kind,
                    });
                }
                RefineRewriteActionKind::MarkStale => {
                    let Some(relative_path) = action.target_relative_path else {
                        return Err(RefineRewriteScopeError::UnmappedImpact);
                    };
                    record_path_intent(
                        &mut path_intents,
                        &relative_path,
                        RewritePathIntent::MarkStale,
                    )?;
                    let reason = action
                        .rationale
                        .unwrap_or_else(|| decision_rationale.clone());
                    scope.stale_descendants.push(RefineStaleDescendant {
                        relative_path,
                        node_id: None,
                        reason,
                    });
                }
                RefineRewriteActionKind::CreateNode => {
                    let Some(relative_path) = action.target_relative_path else {
                        return Err(RefineRewriteScopeError::UnmappedImpact);
                    };
                    record_path_intent(
                        &mut path_intents,
                        &relative_path,
                        RewritePathIntent::Create,
                    )?;
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
                    record_path_intent(
                        &mut path_intents,
                        &relative_path,
                        RewritePathIntent::Remove,
                    )?;
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
                        match action
                            .link_change
                            .unwrap_or(RefineRewriteLinkChangeKind::AddChildLink)
                        {
                            RefineRewriteLinkChangeKind::AddChildLink => {
                                link_change.add_child_relative_paths.push(target);
                            }
                            RefineRewriteLinkChangeKind::RemoveChildLink => {
                                link_change.remove_child_relative_paths.push(target);
                            }
                        }
                    }
                    scope.link_changes.push(link_change);
                }
            }
        }
    }

    Ok(scope)
}

fn record_path_intent(
    path_intents: &mut HashMap<String, RewritePathIntent>,
    relative_path: &str,
    intent: RewritePathIntent,
) -> Result<(), RefineRewriteScopeError> {
    match path_intents.get(relative_path).copied() {
        Some(RewritePathIntent::Update) if intent == RewritePathIntent::Update => Ok(()),
        Some(RewritePathIntent::MarkStale) if intent == RewritePathIntent::MarkStale => Ok(()),
        Some(existing) if existing == intent => {
            Err(RefineRewriteScopeError::ConflictingRewriteTargets)
        }
        Some(_) => Err(RefineRewriteScopeError::ConflictingRewriteTargets),
        None => {
            path_intents.insert(relative_path.to_owned(), intent);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::refine::{
        RefineAffectedScope, RefineDecisionConfirmation, RefineRewriteAction,
        RefineRewriteLinkChangeKind,
    };

    #[test]
    fn scope_routes_mark_stale_actions_to_stale_descendants() {
        let plan_root = temp_plan_root();
        let scope = plan_refine_rewrite_scope(RefineRewriteScopeRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            decisions: vec![confirmed_decision(vec![RefineRewriteAction {
                action_kind: RefineRewriteActionKind::MarkStale,
                target_relative_path: Some("api/old-child.md".to_owned()),
                parent_relative_path: Some("api/api.md".to_owned()),
                node_kind: Some("leaf".to_owned()),
                link_change: None,
                replacement_markdown: None,
                rationale: Some("parent contract changed".to_owned()),
            }])],
        })
        .expect("scope planning should pass");

        assert!(scope.rewrite_targets.is_empty());
        assert_eq!(
            scope.stale_descendants,
            vec![RefineStaleDescendant {
                relative_path: "api/old-child.md".to_owned(),
                node_id: None,
                reason: "parent contract changed".to_owned(),
            }]
        );
    }

    #[test]
    fn scope_respects_update_link_change_kind() {
        let plan_root = temp_plan_root();
        let scope = plan_refine_rewrite_scope(RefineRewriteScopeRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            decisions: vec![confirmed_decision(vec![
                RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::UpdateLinks,
                    target_relative_path: Some("api/new-child.md".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    node_kind: Some("leaf".to_owned()),
                    link_change: Some(RefineRewriteLinkChangeKind::AddChildLink),
                    replacement_markdown: None,
                    rationale: None,
                },
                RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::UpdateLinks,
                    target_relative_path: Some("api/old-child.md".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    node_kind: Some("leaf".to_owned()),
                    link_change: Some(RefineRewriteLinkChangeKind::RemoveChildLink),
                    replacement_markdown: None,
                    rationale: None,
                },
            ])],
        })
        .expect("scope planning should pass");

        assert_eq!(
            scope.link_changes,
            vec![
                RefineLinkChange {
                    parent_relative_path: "api/api.md".to_owned(),
                    add_child_relative_paths: vec!["api/new-child.md".to_owned()],
                    remove_child_relative_paths: Vec::new(),
                },
                RefineLinkChange {
                    parent_relative_path: "api/api.md".to_owned(),
                    add_child_relative_paths: Vec::new(),
                    remove_child_relative_paths: vec!["api/old-child.md".to_owned()],
                },
            ]
        );
    }

    #[test]
    fn scope_rejects_contradictory_actions_for_same_path() {
        let plan_root = temp_plan_root();
        let error = plan_refine_rewrite_scope(RefineRewriteScopeRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            decisions: vec![confirmed_decision(vec![
                RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::UpdateExistingNode,
                    target_relative_path: Some("api/delete-me.md".to_owned()),
                    parent_relative_path: None,
                    node_kind: Some("leaf".to_owned()),
                    link_change: None,
                    replacement_markdown: Some("# Delete Me\n\nUpdated\n".to_owned()),
                    rationale: None,
                },
                RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::RemoveNode,
                    target_relative_path: Some("api/delete-me.md".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    node_kind: Some("leaf".to_owned()),
                    link_change: None,
                    replacement_markdown: None,
                    rationale: None,
                },
            ])],
        })
        .expect_err("update and remove for the same path should fail closed");

        assert_eq!(error, RefineRewriteScopeError::ConflictingRewriteTargets);
    }

    fn confirmed_decision(rewrite_actions: Vec<RefineRewriteAction>) -> RefineDecision {
        RefineDecision {
            source_comments: Vec::new(),
            affected_scope: RefineAffectedScope::default(),
            change_types: Vec::new(),
            confirmation: RefineDecisionConfirmation {
                status: RefineDecisionStatus::ConfirmationCleared,
                rationale: "confirmed".to_owned(),
                question_for_user: None,
                decision_impact: None,
            },
            rewrite_actions,
            expected_gate_revalidation: Vec::new(),
        }
    }

    fn temp_plan_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("loopy-refine-scope-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
