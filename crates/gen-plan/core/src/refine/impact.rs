use serde::{Deserialize, Serialize};

use super::decision::{
    RefineAffectedScope, RefineCommentSource, RefineDecision, RefineDecisionChangeType,
    RefineDecisionConfirmation, RefineDecisionStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineImpactAnalysisRequest {
    pub plan_id: String,
    pub comments: Vec<RefineCommentSource>,
    pub known_node_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineImpactAnalysis {
    pub decisions: Vec<RefineDecision>,
    pub mapping_issues: Vec<RefineImpactMappingIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineImpactMappingIssue {
    pub source_path: String,
    pub category: RefineImpactCategory,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineImpactCategory {
    NodeContentChange,
    ParentContractChange,
    ChildSetChange,
    NewNodeRequest,
    NodeRemovalRequest,
    AmbiguousMapping,
    UserOwnedDecision,
}

pub fn analyze_refine_comment_impact(request: RefineImpactAnalysisRequest) -> RefineImpactAnalysis {
    let mut decisions = Vec::new();
    let mut mapping_issues = Vec::new();

    for comment in request.comments {
        let source_path = comment.source_path.clone();
        let category = classify_comment(&comment);
        let known_path = request
            .known_node_paths
            .iter()
            .any(|path| path == &comment.source_path);
        let ambiguous = !known_path;
        if ambiguous {
            mapping_issues.push(RefineImpactMappingIssue {
                source_path: source_path.clone(),
                category: RefineImpactCategory::AmbiguousMapping,
                message: "comment source path is not present in the known tracked node paths"
                    .to_owned(),
            });
        }
        let status = if ambiguous {
            RefineDecisionStatus::AwaitingManualConfirmation
        } else {
            RefineDecisionStatus::AutoContinuationCandidate
        };
        decisions.push(RefineDecision {
            source_comments: vec![comment],
            affected_scope: RefineAffectedScope {
                affected_files: vec![source_path.clone()],
                affected_tracked_nodes: Vec::new(),
                affected_subtree_roots: Vec::new(),
                unresolved_mapping_note: ambiguous
                    .then(|| "source comment path has no tracked node match".to_owned()),
            },
            change_types: vec![change_type_for_category(&category)],
            confirmation: RefineDecisionConfirmation {
                status,
                rationale: format!("mapped natural-language comment to {category:?}"),
                question_for_user: ambiguous.then(|| {
                    "Which tracked plan node should this refine comment modify?".to_owned()
                }),
                decision_impact: ambiguous.then(|| {
                    "Rewrite scope and gate targets cannot be selected safely.".to_owned()
                }),
            },
            rewrite_actions: Vec::new(),
            expected_gate_revalidation: Vec::new(),
        });
    }

    RefineImpactAnalysis {
        decisions,
        mapping_issues,
    }
}

fn classify_comment(comment: &RefineCommentSource) -> RefineImpactCategory {
    let text = comment
        .comment_text
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if text.contains("new node") || text.contains("add node") {
        RefineImpactCategory::NewNodeRequest
    } else if text.contains("remove node") || text.contains("delete node") {
        RefineImpactCategory::NodeRemovalRequest
    } else if text.contains("child") || text.contains("link") {
        RefineImpactCategory::ChildSetChange
    } else if text.contains("parent") || text.contains("contract") {
        RefineImpactCategory::ParentContractChange
    } else if text.contains("ask user") || text.contains("user decides") {
        RefineImpactCategory::UserOwnedDecision
    } else {
        RefineImpactCategory::NodeContentChange
    }
}

fn change_type_for_category(category: &RefineImpactCategory) -> RefineDecisionChangeType {
    match category {
        RefineImpactCategory::NodeContentChange => RefineDecisionChangeType::InPlaceTextUpdate,
        RefineImpactCategory::ParentContractChange => {
            RefineDecisionChangeType::ParentContractChange
        }
        RefineImpactCategory::ChildSetChange => RefineDecisionChangeType::LinkChange,
        RefineImpactCategory::NewNodeRequest => RefineDecisionChangeType::NodeCreation,
        RefineImpactCategory::NodeRemovalRequest => RefineDecisionChangeType::NodeRemoval,
        RefineImpactCategory::AmbiguousMapping | RefineImpactCategory::UserOwnedDecision => {
            RefineDecisionChangeType::ContextInvalidation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impact_analysis_maps_comments_and_detects_ambiguity() {
        let analysis = analyze_refine_comment_impact(RefineImpactAnalysisRequest {
            plan_id: "plan-1".to_owned(),
            comments: vec![
                RefineCommentSource {
                    source_path: "api/add-auth-tests.md".to_owned(),
                    begin_comment_line: 1,
                    end_comment_line: 3,
                    comment_text: Some("tighten content".to_owned()),
                },
                RefineCommentSource {
                    source_path: "missing.md".to_owned(),
                    begin_comment_line: 5,
                    end_comment_line: 7,
                    comment_text: Some("add node".to_owned()),
                },
            ],
            known_node_paths: vec!["api/add-auth-tests.md".to_owned()],
        });

        assert_eq!(analysis.decisions.len(), 2);
        assert_eq!(analysis.mapping_issues.len(), 1);
        assert_eq!(
            analysis.decisions[0].change_types,
            vec![RefineDecisionChangeType::InPlaceTextUpdate]
        );
        assert_eq!(
            analysis.decisions[1].confirmation.status,
            RefineDecisionStatus::AwaitingManualConfirmation
        );
    }
}
