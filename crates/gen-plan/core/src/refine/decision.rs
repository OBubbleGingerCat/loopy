use serde::{Deserialize, Serialize};

/// Refine Decision Model.
///
/// Refine decisions are distinct from runtime gate results. They describe expected
/// rewrite and revalidation consequences before any runtime gate is executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineDecision {
    pub source_comments: Vec<RefineCommentSource>,
    pub affected_scope: RefineAffectedScope,
    pub change_types: Vec<RefineDecisionChangeType>,
    pub confirmation: RefineDecisionConfirmation,
    pub rewrite_actions: Vec<RefineRewriteAction>,
    pub expected_gate_revalidation: Vec<ExpectedGateRevalidation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineCommentSource {
    pub source_path: String,
    pub begin_comment_line: usize,
    pub end_comment_line: usize,
    pub comment_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineAffectedTrackedNode {
    pub node_id: String,
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RefineAffectedScope {
    pub affected_files: Vec<String>,
    pub affected_tracked_nodes: Vec<RefineAffectedTrackedNode>,
    pub affected_subtree_roots: Vec<String>,
    pub unresolved_mapping_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineDecisionChangeType {
    InPlaceTextUpdate,
    NodeCreation,
    NodeRemoval,
    LinkChange,
    ParentContractChange,
    ContextInvalidation,
    StaleDescendant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineDecisionStatus {
    AwaitingManualConfirmation,
    AutoContinuationCandidate,
    UserDecisionBlocked,
    ConfirmationCleared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineDecisionConfirmation {
    pub status: RefineDecisionStatus,
    pub rationale: String,
    pub question_for_user: Option<String>,
    pub decision_impact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineRewriteActionKind {
    UpdateExistingNode,
    CreateNode,
    RemoveNode,
    MarkStale,
    UpdateLinks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineRewriteLinkChangeKind {
    AddChildLink,
    RemoveChildLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRewriteAction {
    pub action_kind: RefineRewriteActionKind,
    pub target_relative_path: Option<String>,
    pub parent_relative_path: Option<String>,
    pub node_kind: Option<String>,
    pub link_change: Option<RefineRewriteLinkChangeKind>,
    pub replacement_markdown: Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExpectedGateRevalidation {
    pub leaf_targets: Vec<String>,
    pub frontier_targets: Vec<String>,
    pub reasons: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refine_decision_records_source_scope_confirmation_and_expected_gates() {
        let decision = RefineDecision {
            source_comments: vec![RefineCommentSource {
                source_path: "api/add-auth-tests.md".to_owned(),
                begin_comment_line: 10,
                end_comment_line: 12,
                comment_text: Some("tighten verification".to_owned()),
            }],
            affected_scope: RefineAffectedScope {
                affected_files: vec!["api/add-auth-tests.md".to_owned()],
                affected_tracked_nodes: vec![RefineAffectedTrackedNode {
                    node_id: "leaf-1".to_owned(),
                    relative_path: Some("api/add-auth-tests.md".to_owned()),
                }],
                affected_subtree_roots: vec!["api/api.md".to_owned()],
                unresolved_mapping_note: None,
            },
            change_types: vec![RefineDecisionChangeType::InPlaceTextUpdate],
            confirmation: RefineDecisionConfirmation {
                status: RefineDecisionStatus::ConfirmationCleared,
                rationale: "comment maps to one leaf".to_owned(),
                question_for_user: None,
                decision_impact: None,
            },
            rewrite_actions: vec![RefineRewriteAction {
                action_kind: RefineRewriteActionKind::UpdateExistingNode,
                target_relative_path: Some("api/add-auth-tests.md".to_owned()),
                parent_relative_path: Some("api/api.md".to_owned()),
                node_kind: Some("leaf".to_owned()),
                link_change: None,
                replacement_markdown: Some("# Add Auth Tests\n".to_owned()),
                rationale: Some("apply natural-language feedback".to_owned()),
            }],
            expected_gate_revalidation: vec![ExpectedGateRevalidation {
                leaf_targets: vec!["api/add-auth-tests.md".to_owned()],
                frontier_targets: vec!["api/api.md".to_owned()],
                reasons: vec!["text changed".to_owned()],
            }],
        };

        assert_eq!(decision.source_comments[0].begin_comment_line, 10);
        assert_eq!(
            decision.affected_scope.affected_tracked_nodes[0].node_id,
            "leaf-1"
        );
        assert_eq!(
            decision.confirmation.status,
            RefineDecisionStatus::ConfirmationCleared
        );
        assert_eq!(
            decision.expected_gate_revalidation[0].leaf_targets,
            vec!["api/add-auth-tests.md"]
        );
    }
}
