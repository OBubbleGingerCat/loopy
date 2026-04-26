use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::runtime::comments::{CommentDiscoveryError, parse_comment_blocks_for_file};

use super::decision::{
    ExpectedGateRevalidation, RefineDecision, RefineDecisionStatus, RefineRewriteActionKind,
};
use super::scope::{RefineRewriteScope, RefineStaleDescendant};
use super::summary::RefineRewriteSummary;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRewriteRequest {
    pub plan_id: String,
    pub plan_root: PathBuf,
    pub decisions: Vec<RefineDecision>,
    pub rewrite_scope: RefineRewriteScope,
    pub blocked_follow_ups: Vec<RefineDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineRewriteResult {
    pub changed_files: Vec<RefineChangedFile>,
    pub structural_changes: Vec<RefineStructuralChange>,
    pub stale_nodes: Vec<RefineStaleNode>,
    pub context_invalidations: Vec<RefineContextInvalidation>,
    pub unchanged_nodes: Vec<RefineUnchangedNode>,
    pub expected_gate_targets: Vec<ExpectedGateRevalidation>,
    pub unresolved_follow_ups: Vec<RefineDecision>,
    pub summary: RefineRewriteSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineChangedFile {
    pub relative_path: String,
    pub node_id: Option<String>,
    pub change_kind: RefineChangedFileKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineChangedFileKind {
    TextUpdated,
    Created,
    Regenerated,
    StaleMarked,
    ExplicitlyRemoved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineStructuralChange {
    pub parent_relative_path: String,
    pub parent_node_id: Option<String>,
    pub change_kind: RefineStructuralChangeKind,
    pub added_child_relative_paths: Vec<String>,
    pub removed_child_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineStructuralChangeKind {
    ChangedChildSet,
    ParentContractChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineStaleNode {
    pub relative_path: String,
    pub node_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineContextInvalidation {
    pub relative_path: String,
    pub node_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineUnchangedNode {
    pub relative_path: String,
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefineRewriteError {
    MissingPlanRoot,
    InvalidPlanRoot,
    UnauthorizedDecision,
    InvalidRewriteScope,
    Io {
        path: String,
        source: String,
    },
    PostWriteStructureViolation {
        relative_path: String,
        reason: String,
    },
}

impl fmt::Display for RefineRewriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RefineRewriteError {}

#[derive(Default)]
struct MutationReports {
    changed_files: Vec<RefineChangedFile>,
    structural_changes: Vec<RefineStructuralChange>,
    stale_nodes: Vec<RefineStaleNode>,
    context_invalidations: Vec<RefineContextInvalidation>,
    unchanged_nodes: Vec<RefineUnchangedNode>,
}

pub fn apply_refine_rewrite(
    request: RefineRewriteRequest,
) -> Result<RefineRewriteResult, RefineRewriteError> {
    // Fixed order; stop on the first error before later mutation steps.
    ensure_rewrite_application_confirmed(&request)?;
    validate_refine_rewrite_request(&request)?;
    let sanitized_payloads = cleanup_processed_comment_blocks(&request)?;
    let mut reports = MutationReports::default();
    apply_in_place_node_rewrites(&request, &sanitized_payloads, &mut reports)?;
    create_new_node_files(&request, &sanitized_payloads, &mut reports)?;
    apply_explicit_removals(&request, &mut reports)?;
    mark_stale_nodes(&request, &mut reports)?;
    apply_rewrite_link_updates(&request, &mut reports)?;
    run_post_write_structural_checks(&request)?;
    assemble_refine_rewrite_result(&request, reports)
}

fn ensure_rewrite_application_confirmed(
    request: &RefineRewriteRequest,
) -> Result<(), RefineRewriteError> {
    // rewrite application confirmation guard
    for decision in &request.decisions {
        if matches!(
            decision.confirmation.status,
            RefineDecisionStatus::AwaitingManualConfirmation
                | RefineDecisionStatus::UserDecisionBlocked
        ) {
            return Err(RefineRewriteError::UnauthorizedDecision);
        }
    }
    Ok(())
}

fn validate_refine_rewrite_request(
    request: &RefineRewriteRequest,
) -> Result<(), RefineRewriteError> {
    if request.plan_root.as_os_str().is_empty() {
        return Err(RefineRewriteError::MissingPlanRoot);
    }
    if !request.plan_root.is_dir() {
        return Err(RefineRewriteError::InvalidPlanRoot);
    }
    if !request.rewrite_scope.conflicts.is_empty() {
        return Err(RefineRewriteError::InvalidRewriteScope);
    }
    for target in &request.rewrite_scope.rewrite_targets {
        validate_plan_relative_markdown_path(&target.relative_path)?;
    }
    for creation in &request.rewrite_scope.node_creations {
        validate_plan_relative_markdown_path(&creation.relative_path)?;
    }
    for removal in &request.rewrite_scope.node_removals {
        validate_plan_relative_markdown_path(&removal.relative_path)?;
    }
    Ok(())
}

fn cleanup_processed_comment_blocks(
    request: &RefineRewriteRequest,
) -> Result<Vec<(String, String)>, RefineRewriteError> {
    // processed comment block cleanup
    let mut sanitized_payloads = Vec::new();
    for target in &request.rewrite_scope.rewrite_targets {
        let path = request.plan_root.join(&target.relative_path);
        if !path.is_file() {
            continue;
        }
        let markdown = fs::read_to_string(&path).map_err(|source| RefineRewriteError::Io {
            path: path.display().to_string(),
            source: source.to_string(),
        })?;
        let sanitized = remove_comment_blocks(&target.relative_path, &markdown)?;
        sanitized_payloads.push((target.relative_path.clone(), sanitized));
    }
    Ok(sanitized_payloads)
}

fn apply_in_place_node_rewrites(
    request: &RefineRewriteRequest,
    sanitized_payloads: &[(String, String)],
    reports: &mut MutationReports,
) -> Result<(), RefineRewriteError> {
    // in-place node rewrite rules
    for target in &request.rewrite_scope.rewrite_targets {
        if target.action_kind != RefineRewriteActionKind::UpdateExistingNode {
            continue;
        }
        let replacement = replacement_for_path(request, &target.relative_path).or_else(|| {
            sanitized_payloads
                .iter()
                .find(|(path, _)| path == &target.relative_path)
                .map(|(_, text)| text.clone())
        });
        let Some(markdown) = replacement else {
            reports.unchanged_nodes.push(RefineUnchangedNode {
                relative_path: target.relative_path.clone(),
                node_id: target.node_id.clone(),
            });
            continue;
        };
        write_plan_file(&request.plan_root, &target.relative_path, &markdown)?;
        reports.changed_files.push(RefineChangedFile {
            relative_path: target.relative_path.clone(),
            node_id: target.node_id.clone(),
            change_kind: RefineChangedFileKind::TextUpdated,
        });
    }
    Ok(())
}

fn create_new_node_files(
    request: &RefineRewriteRequest,
    _sanitized_payloads: &[(String, String)],
    reports: &mut MutationReports,
) -> Result<(), RefineRewriteError> {
    // new node file creation rules
    for creation in &request.rewrite_scope.node_creations {
        let markdown = replacement_for_path(request, &creation.relative_path)
            .unwrap_or_else(|| format!("# {}\n", title_from_path(&creation.relative_path)));
        write_plan_file(&request.plan_root, &creation.relative_path, &markdown)?;
        reports.changed_files.push(RefineChangedFile {
            relative_path: creation.relative_path.clone(),
            node_id: None,
            change_kind: RefineChangedFileKind::Created,
        });
    }
    Ok(())
}

fn apply_explicit_removals(
    request: &RefineRewriteRequest,
    reports: &mut MutationReports,
) -> Result<(), RefineRewriteError> {
    // explicit removal application rules
    for removal in &request.rewrite_scope.node_removals {
        let path = request.plan_root.join(&removal.relative_path);
        if path.exists() {
            fs::remove_file(&path).map_err(|source| RefineRewriteError::Io {
                path: path.display().to_string(),
                source: source.to_string(),
            })?;
        }
        reports.changed_files.push(RefineChangedFile {
            relative_path: removal.relative_path.clone(),
            node_id: removal.node_id.clone(),
            change_kind: RefineChangedFileKind::ExplicitlyRemoved,
        });
    }
    Ok(())
}

fn mark_stale_nodes(
    request: &RefineRewriteRequest,
    reports: &mut MutationReports,
) -> Result<(), RefineRewriteError> {
    // stale node handling rules
    for stale in &request.rewrite_scope.stale_descendants {
        mark_single_stale_node(request, reports, stale)?;
    }
    Ok(())
}

fn mark_single_stale_node(
    request: &RefineRewriteRequest,
    reports: &mut MutationReports,
    stale: &RefineStaleDescendant,
) -> Result<(), RefineRewriteError> {
    let path = request.plan_root.join(&stale.relative_path);
    if path.is_file() {
        let mut markdown = fs::read_to_string(&path).map_err(|source| RefineRewriteError::Io {
            path: path.display().to_string(),
            source: source.to_string(),
        })?;
        if !markdown.contains("## Refine Stale") {
            markdown.push_str(&format!("\n\n## Refine Stale\n{}\n", stale.reason));
            write_plan_file(&request.plan_root, &stale.relative_path, &markdown)?;
            reports.changed_files.push(RefineChangedFile {
                relative_path: stale.relative_path.clone(),
                node_id: stale.node_id.clone(),
                change_kind: RefineChangedFileKind::StaleMarked,
            });
        }
    }
    reports.stale_nodes.push(RefineStaleNode {
        relative_path: stale.relative_path.clone(),
        node_id: stale.node_id.clone(),
        reason: stale.reason.clone(),
    });
    Ok(())
}

fn apply_rewrite_link_updates(
    request: &RefineRewriteRequest,
    reports: &mut MutationReports,
) -> Result<(), RefineRewriteError> {
    // rewrite link update rules
    for change in &request.rewrite_scope.link_changes {
        validate_plan_relative_markdown_path(&change.parent_relative_path)?;
        reports.structural_changes.push(RefineStructuralChange {
            parent_relative_path: change.parent_relative_path.clone(),
            parent_node_id: None,
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: stable_dedup(&change.add_child_relative_paths),
            removed_child_relative_paths: stable_dedup(&change.remove_child_relative_paths),
        });
    }
    Ok(())
}

fn run_post_write_structural_checks(
    request: &RefineRewriteRequest,
) -> Result<(), RefineRewriteError> {
    // post-write structural checks
    for changed in request
        .rewrite_scope
        .rewrite_targets
        .iter()
        .map(|target| target.relative_path.as_str())
        .chain(
            request
                .rewrite_scope
                .node_creations
                .iter()
                .map(|target| target.relative_path.as_str()),
        )
    {
        let path = request.plan_root.join(changed);
        if path.exists() {
            let markdown = fs::read_to_string(&path).map_err(|source| RefineRewriteError::Io {
                path: path.display().to_string(),
                source: source.to_string(),
            })?;
            if !markdown.trim_start().starts_with('#') {
                return Err(RefineRewriteError::PostWriteStructureViolation {
                    relative_path: changed.to_owned(),
                    reason: "required plan headings were lost".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn assemble_refine_rewrite_result(
    request: &RefineRewriteRequest,
    reports: MutationReports,
) -> Result<RefineRewriteResult, RefineRewriteError> {
    // created_nodes and removed_nodes are intentionally not separate top-level result buckets.
    // actual runtime gate pass/fail results are intentionally absent from RefineRewriteResult.
    let mut expected_gate_targets = Vec::new();
    for decision in &request.decisions {
        expected_gate_targets.extend(decision.expected_gate_revalidation.clone());
    }
    let summary = RefineRewriteSummary {
        changed_file_count: reports.changed_files.len(),
        structural_change_count: reports.structural_changes.len(),
        stale_node_count: reports.stale_nodes.len(),
        context_invalidation_count: reports.context_invalidations.len(),
        unchanged_node_count: reports.unchanged_nodes.len(),
        expected_leaf_gate_target_count: expected_gate_targets
            .iter()
            .map(|target| target.leaf_targets.len())
            .sum(),
        expected_frontier_gate_target_count: expected_gate_targets
            .iter()
            .map(|target| target.frontier_targets.len())
            .sum(),
        unresolved_follow_up_count: request.blocked_follow_ups.len(),
    };
    Ok(RefineRewriteResult {
        changed_files: reports.changed_files,
        structural_changes: reports.structural_changes,
        stale_nodes: reports.stale_nodes,
        context_invalidations: reports.context_invalidations,
        unchanged_nodes: reports.unchanged_nodes,
        expected_gate_targets,
        unresolved_follow_ups: request.blocked_follow_ups.clone(),
        summary,
    })
}

fn replacement_for_path(request: &RefineRewriteRequest, relative_path: &str) -> Option<String> {
    request
        .decisions
        .iter()
        .flat_map(|decision| decision.rewrite_actions.iter())
        .find(|action| action.target_relative_path.as_deref() == Some(relative_path))
        .and_then(|action| action.replacement_markdown.clone())
}

fn remove_comment_blocks(
    relative_path: &str,
    markdown: &str,
) -> Result<String, RefineRewriteError> {
    parse_comment_blocks_for_file(relative_path, markdown).map_err(map_comment_error)?;
    let mut output = Vec::new();
    let mut inside = false;
    for line in markdown.lines() {
        match line.trim() {
            "BEGIN_COMMENT" => inside = true,
            "END_COMMENT" => inside = false,
            _ if !inside => output.push(line),
            _ => {}
        }
    }
    Ok(output.join("\n"))
}

fn map_comment_error(error: CommentDiscoveryError) -> RefineRewriteError {
    RefineRewriteError::PostWriteStructureViolation {
        relative_path: match &error {
            CommentDiscoveryError::MalformedStructure { relative_path, .. }
            | CommentDiscoveryError::Io { relative_path, .. } => relative_path.clone(),
            CommentDiscoveryError::InvalidPlanRoot { plan_root } => plan_root.display().to_string(),
        },
        reason: error.to_string(),
    }
}

fn write_plan_file(
    plan_root: &Path,
    relative_path: &str,
    markdown: &str,
) -> Result<(), RefineRewriteError> {
    validate_plan_relative_markdown_path(relative_path)?;
    let full_path = plan_root.join(relative_path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).map_err(|source| RefineRewriteError::Io {
            path: parent.display().to_string(),
            source: source.to_string(),
        })?;
    }
    fs::write(&full_path, markdown).map_err(|source| RefineRewriteError::Io {
        path: full_path.display().to_string(),
        source: source.to_string(),
    })
}

fn validate_plan_relative_markdown_path(relative_path: &str) -> Result<(), RefineRewriteError> {
    let path = Path::new(relative_path);
    if relative_path.is_empty()
        || path.is_absolute()
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return Err(RefineRewriteError::InvalidRewriteScope);
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(RefineRewriteError::InvalidRewriteScope);
        }
    }
    Ok(())
}

fn stable_dedup(values: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(value) {
            result.push(value.clone());
        }
    }
    result
}

fn title_from_path(relative_path: &str) -> String {
    Path::new(relative_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("refined-node")
        .split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::refine::{
        RefineDecisionConfirmation, RefineRewriteAction, RefineRewriteActionKind,
        RefineRewriteTarget,
    };

    #[test]
    fn refine_rewrite_orchestration_sequences_mutations_and_result_buckets() {
        let plan_root = temp_plan_root();
        fs::write(
            plan_root.join("api.md"),
            "# API\n\nBEGIN_COMMENT\nChange text\nEND_COMMENT\n",
        )
        .unwrap();
        let decision = RefineDecision {
            source_comments: Vec::new(),
            affected_scope: Default::default(),
            change_types: vec![],
            confirmation: RefineDecisionConfirmation {
                status: RefineDecisionStatus::ConfirmationCleared,
                rationale: "confirmed".to_owned(),
                question_for_user: None,
                decision_impact: None,
            },
            rewrite_actions: vec![RefineRewriteAction {
                action_kind: RefineRewriteActionKind::UpdateExistingNode,
                target_relative_path: Some("api.md".to_owned()),
                parent_relative_path: None,
                node_kind: Some("leaf".to_owned()),
                link_change: None,
                replacement_markdown: Some("# API\n\nUpdated\n".to_owned()),
                rationale: None,
            }],
            expected_gate_revalidation: vec![ExpectedGateRevalidation {
                leaf_targets: vec!["api.md".to_owned()],
                frontier_targets: vec![],
                reasons: vec!["text".to_owned()],
            }],
        };
        let result = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![decision.clone()],
            rewrite_scope: RefineRewriteScope {
                rewrite_targets: vec![RefineRewriteTarget {
                    relative_path: "api.md".to_owned(),
                    node_id: Some("leaf-1".to_owned()),
                    action_kind: RefineRewriteActionKind::UpdateExistingNode,
                }],
                ..Default::default()
            },
            blocked_follow_ups: vec![decision],
        })
        .expect("rewrite should pass");

        assert_eq!(
            fs::read_to_string(plan_root.join("api.md")).unwrap(),
            "# API\n\nUpdated\n"
        );
        assert_eq!(
            result.changed_files[0].change_kind,
            RefineChangedFileKind::TextUpdated
        );
        assert_eq!(result.unresolved_follow_ups.len(), 1);
        assert_eq!(result.summary.expected_leaf_gate_target_count, 1);

        let blocked = RefineDecision {
            confirmation: RefineDecisionConfirmation {
                status: RefineDecisionStatus::AwaitingManualConfirmation,
                rationale: "manual".to_owned(),
                question_for_user: None,
                decision_impact: None,
            },
            ..result.unresolved_follow_ups[0].clone()
        };
        let error = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            decisions: vec![blocked],
            rewrite_scope: RefineRewriteScope::default(),
            blocked_follow_ups: Vec::new(),
        })
        .expect_err("unconfirmed decision should fail before mutations");
        assert_eq!(error, RefineRewriteError::UnauthorizedDecision);
    }

    fn temp_plan_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("loopy-refine-rewrite-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
