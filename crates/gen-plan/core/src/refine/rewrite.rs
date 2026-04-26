use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::runtime::comments::{
    CommentBlock, CommentDiscoveryError, parse_comment_blocks_for_file,
};

use super::decision::{
    ExpectedGateRevalidation, RefineCommentSource, RefineDecision, RefineDecisionStatus,
    RefineRewriteActionKind,
};
use super::scope::{RefineLinkChange, RefineRewriteScope, RefineStaleDescendant};
use super::summary::{RefineRewriteSummary, build_refine_rewrite_summary};

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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineStaleMarkReport {
    pub changed_files: Vec<RefineChangedFile>,
    pub stale_nodes: Vec<RefineStaleNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineLinkUpdateReport {
    pub changed_parent_files: Vec<RefineChangedFile>,
    pub structural_changes: Vec<RefineStructuralChange>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreservedCommentBlock {
    start_line: usize,
    end_line: usize,
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
    let stale_report = mark_stale_nodes(&request, &sanitized_payloads)?;
    reports.changed_files.extend(stale_report.changed_files);
    reports.stale_nodes.extend(stale_report.stale_nodes);
    let link_report = apply_rewrite_link_updates(&request, &sanitized_payloads)?;
    reports
        .changed_files
        .extend(link_report.changed_parent_files);
    reports
        .structural_changes
        .extend(link_report.structural_changes);
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
    for stale in &request.rewrite_scope.stale_descendants {
        validate_plan_relative_markdown_path(&stale.relative_path)?;
    }
    for change in &request.rewrite_scope.link_changes {
        validate_plan_relative_markdown_path(&change.parent_relative_path)?;
        for child in change
            .add_child_relative_paths
            .iter()
            .chain(change.remove_child_relative_paths.iter())
        {
            validate_plan_relative_markdown_path(child)?;
        }
    }
    for source in request
        .decisions
        .iter()
        .flat_map(|decision| decision.source_comments.iter())
    {
        validate_plan_relative_markdown_path(&source.source_path)?;
    }
    prevalidate_rewrite_link_updates(request)?;
    Ok(())
}

fn prevalidate_rewrite_link_updates(
    request: &RefineRewriteRequest,
) -> Result<(), RefineRewriteError> {
    let merged_changes = merge_link_changes(&request.rewrite_scope.link_changes)?;
    let planned_creations = request
        .rewrite_scope
        .node_creations
        .iter()
        .map(|creation| creation.relative_path.as_str())
        .collect::<HashSet<_>>();
    for change in merged_changes {
        if !path_exists_or_is_planned_creation(
            request,
            &change.parent_relative_path,
            &planned_creations,
        ) {
            return Err(RefineRewriteError::InvalidRewriteScope);
        }
        for child in &change.add_child_relative_paths {
            if !path_exists_or_is_planned_creation(request, child, &planned_creations) {
                return Err(RefineRewriteError::InvalidRewriteScope);
            }
        }
    }
    Ok(())
}

fn path_exists_or_is_planned_creation(
    request: &RefineRewriteRequest,
    relative_path: &str,
    planned_creations: &HashSet<&str>,
) -> bool {
    planned_creations.contains(relative_path) || request.plan_root.join(relative_path).is_file()
}

fn cleanup_processed_comment_blocks(
    request: &RefineRewriteRequest,
) -> Result<Vec<(String, String)>, RefineRewriteError> {
    // processed comment block cleanup
    let mut sanitized_payloads = Vec::new();
    for relative_path in comment_cleanup_paths(request) {
        let path = request.plan_root.join(&relative_path);
        if !path.is_file() {
            continue;
        }
        let markdown = fs::read_to_string(&path).map_err(|source| RefineRewriteError::Io {
            path: path.display().to_string(),
            source: source.to_string(),
        })?;
        let preserved_blocks = preserved_comment_blocks_for_path(request, &relative_path);
        let sanitized = remove_comment_blocks(&relative_path, &markdown, &preserved_blocks)?;
        if sanitized != markdown && !path_mutated_after_comment_cleanup(request, &relative_path) {
            write_plan_file(&request.plan_root, &relative_path, &sanitized)?;
        }
        sanitized_payloads.push((relative_path, sanitized));
    }
    Ok(sanitized_payloads)
}

fn comment_cleanup_paths(request: &RefineRewriteRequest) -> Vec<String> {
    let mut paths = Vec::new();
    for target in &request.rewrite_scope.rewrite_targets {
        push_unique_path(&mut paths, &target.relative_path);
    }
    for stale in &request.rewrite_scope.stale_descendants {
        push_unique_path(&mut paths, &stale.relative_path);
    }
    for change in &request.rewrite_scope.link_changes {
        push_unique_path(&mut paths, &change.parent_relative_path);
    }
    for source in request
        .decisions
        .iter()
        .flat_map(|decision| decision.source_comments.iter())
    {
        push_unique_path(&mut paths, &source.source_path);
    }
    paths
}

fn path_mutated_after_comment_cleanup(request: &RefineRewriteRequest, relative_path: &str) -> bool {
    request
        .rewrite_scope
        .rewrite_targets
        .iter()
        .any(|target| target.relative_path == relative_path)
        || request
            .rewrite_scope
            .node_creations
            .iter()
            .any(|creation| creation.relative_path == relative_path)
        || request
            .rewrite_scope
            .node_removals
            .iter()
            .any(|removal| removal.relative_path == relative_path)
        || request
            .rewrite_scope
            .stale_descendants
            .iter()
            .any(|stale| stale.relative_path == relative_path)
        || request
            .rewrite_scope
            .link_changes
            .iter()
            .any(|change| change.parent_relative_path == relative_path)
}

fn push_unique_path(paths: &mut Vec<String>, relative_path: &str) {
    if !paths.iter().any(|path| path == relative_path) {
        paths.push(relative_path.to_owned());
    }
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
        let replacement =
            sanitized_replacement_for_path(request, &target.relative_path)?.or_else(|| {
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
        let markdown = sanitized_replacement_for_path(request, &creation.relative_path)?
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
    sanitized_payloads: &[(String, String)],
) -> Result<RefineStaleMarkReport, RefineRewriteError> {
    // stale node handling rules: request.rewrite_scope.stale_descendants use the selected stale-descendant policy.
    // mark stale node content non-destructively; stale markdown files remain on disk, and physical file removal
    // is reserved for explicit removal decisions. The leading marker uses loopy-refine-status: stale and
    // loopy-refine-stale-reason, must never duplicate stale marker blocks, every --> sequence is replaced with -- >,
    // and the stale reason is not rejected solely because it originally contained -->. The helper must
    // preserve the rest of the markdown content byte-for-byte. Stale node handling follows the revision invalidation rule.
    let mut report = RefineStaleMarkReport::default();
    for stale in deduplicate_stale_descendants(&request.rewrite_scope.stale_descendants)? {
        let path = request.plan_root.join(&stale.relative_path);
        if !path.is_file() {
            return Err(RefineRewriteError::InvalidRewriteScope);
        }
        let markdown =
            markdown_from_sanitized_or_disk(request, sanitized_payloads, &stale.relative_path)?;
        let marked = apply_stale_marker(&markdown, &stale.reason);
        write_plan_file(&request.plan_root, &stale.relative_path, &marked)?;
        report.changed_files.push(RefineChangedFile {
            relative_path: stale.relative_path.clone(),
            node_id: stale.node_id.clone(),
            change_kind: RefineChangedFileKind::StaleMarked,
        });
        report.stale_nodes.push(RefineStaleNode {
            relative_path: stale.relative_path,
            node_id: stale.node_id,
            reason: stale.reason,
        });
    }
    Ok(report)
}

fn apply_rewrite_link_updates(
    request: &RefineRewriteRequest,
    sanitized_payloads: &[(String, String)],
) -> Result<RefineLinkUpdateReport, RefineRewriteError> {
    // rewrite link update rules: request.rewrite_scope.link_changes are merged in stable first-seen parent order.
    // This helper owns all parent Child Nodes section mutations. Add-link targets must exist as canonical markdown files,
    // Remove-link targets may already be absent, child-node links point to actual markdown files, relative paths are used,
    // added labels come from first # heading text, newly created parent Child Nodes sections are populated here, and the
    // rewrite must not leave broken child links. Existing parent edits report TextUpdated; structural_changes emit
    // ChangedChildSet and ParentContractChanged.
    let mut report = RefineLinkUpdateReport::default();
    let merged_changes = merge_link_changes(&request.rewrite_scope.link_changes)?;
    let created_parent_paths = request
        .rewrite_scope
        .node_creations
        .iter()
        .map(|creation| creation.relative_path.as_str())
        .collect::<HashSet<_>>();

    for change in merged_changes {
        let Some(mutation) =
            apply_single_link_update(request, &change, &created_parent_paths, sanitized_payloads)?
        else {
            continue;
        };
        if !mutation.parent_was_created {
            report.changed_parent_files.push(RefineChangedFile {
                relative_path: mutation.parent_relative_path.clone(),
                node_id: mutation.parent_node_id.clone(),
                change_kind: RefineChangedFileKind::TextUpdated,
            });
        }
        for change_kind in [
            RefineStructuralChangeKind::ChangedChildSet,
            RefineStructuralChangeKind::ParentContractChanged,
        ] {
            report.structural_changes.push(RefineStructuralChange {
                parent_relative_path: mutation.parent_relative_path.clone(),
                parent_node_id: mutation.parent_node_id.clone(),
                change_kind,
                added_child_relative_paths: mutation.added_child_relative_paths.clone(),
                removed_child_relative_paths: mutation.removed_child_relative_paths.clone(),
            });
        }
    }

    Ok(report)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeduplicatedStaleDescendant {
    relative_path: String,
    node_id: Option<String>,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MergedLinkChange {
    parent_relative_path: String,
    add_child_relative_paths: Vec<String>,
    remove_child_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParentLinkMutation {
    parent_relative_path: String,
    parent_node_id: Option<String>,
    parent_was_created: bool,
    added_child_relative_paths: Vec<String>,
    removed_child_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildLinkRow {
    canonical_path: String,
    rendered_line: String,
}

fn deduplicate_stale_descendants(
    stale_descendants: &[RefineStaleDescendant],
) -> Result<Vec<DeduplicatedStaleDescendant>, RefineRewriteError> {
    let mut deduplicated: Vec<DeduplicatedStaleDescendant> = Vec::new();
    for stale in stale_descendants {
        validate_plan_relative_markdown_path(&stale.relative_path)?;
        let reason = normalize_stale_reason(&stale.reason);
        if let Some(existing) = deduplicated
            .iter_mut()
            .find(|entry| entry.relative_path == stale.relative_path)
        {
            if existing.node_id.is_none() {
                existing.node_id = stale.node_id.clone();
            }
            if !reason.is_empty() {
                existing.reason = reason;
            }
            continue;
        }
        deduplicated.push(DeduplicatedStaleDescendant {
            relative_path: stale.relative_path.clone(),
            node_id: stale.node_id.clone(),
            reason,
        });
    }
    Ok(deduplicated)
}

fn normalize_stale_reason(reason: &str) -> String {
    reason
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("-->", "-- >")
}

fn apply_stale_marker(markdown: &str, reason: &str) -> String {
    let stripped = strip_leading_stale_marker(markdown);
    let marker = format!(
        "<!-- loopy-refine-status: stale -->\n<!-- loopy-refine-stale-reason: {reason} -->\n"
    );
    let insert_at = first_markdown_heading_offset(stripped).unwrap_or(0);
    let mut output = String::with_capacity(stripped.len() + marker.len());
    output.push_str(&stripped[..insert_at]);
    output.push_str(&marker);
    output.push_str(&stripped[insert_at..]);
    output
}

fn strip_leading_stale_marker(markdown: &str) -> &str {
    let Some((first_line, first_len)) = next_line(markdown, 0) else {
        return markdown;
    };
    if !is_stale_status_marker_line(first_line) {
        return markdown;
    }
    let mut offset = first_len;
    if let Some((second_line, second_len)) = next_line(markdown, offset)
        && is_stale_reason_marker_line(second_line)
    {
        offset += second_len;
    }
    &markdown[offset..]
}

fn next_line(markdown: &str, offset: usize) -> Option<(&str, usize)> {
    if offset >= markdown.len() {
        return None;
    }
    let rest = &markdown[offset..];
    match rest.find('\n') {
        Some(index) => Some((&rest[..=index], index + 1)),
        None => Some((rest, rest.len())),
    }
}

fn is_stale_status_marker_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("<!-- loopy-refine-status:") && trimmed.ends_with("-->")
}

fn is_stale_reason_marker_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("<!-- loopy-refine-stale-reason:") && trimmed.ends_with("-->")
}

fn first_markdown_heading_offset(markdown: &str) -> Option<usize> {
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            return Some(offset);
        }
        offset += line.len();
    }
    if !markdown.ends_with('\n') {
        let tail_start = markdown.rfind('\n').map(|index| index + 1).unwrap_or(0);
        if markdown[tail_start..].trim_start().starts_with('#') {
            return Some(tail_start);
        }
    }
    None
}

fn merge_link_changes(
    link_changes: &[RefineLinkChange],
) -> Result<Vec<MergedLinkChange>, RefineRewriteError> {
    let mut merged = Vec::<MergedLinkChange>::new();
    for change in link_changes {
        validate_plan_relative_markdown_path(&change.parent_relative_path)?;
        let index = merged
            .iter()
            .position(|existing| existing.parent_relative_path == change.parent_relative_path);
        let merged_change = match index {
            Some(index) => &mut merged[index],
            None => {
                merged.push(MergedLinkChange {
                    parent_relative_path: change.parent_relative_path.clone(),
                    add_child_relative_paths: Vec::new(),
                    remove_child_relative_paths: Vec::new(),
                });
                merged.last_mut().expect("merged change exists")
            }
        };
        append_unique_validated_paths(
            &mut merged_change.add_child_relative_paths,
            &change.add_child_relative_paths,
        )?;
        append_unique_validated_paths(
            &mut merged_change.remove_child_relative_paths,
            &change.remove_child_relative_paths,
        )?;
    }

    for change in &merged {
        if change
            .add_child_relative_paths
            .iter()
            .any(|child| change.remove_child_relative_paths.contains(child))
        {
            return Err(RefineRewriteError::InvalidRewriteScope);
        }
    }

    Ok(merged)
}

fn append_unique_validated_paths(
    target: &mut Vec<String>,
    values: &[String],
) -> Result<(), RefineRewriteError> {
    for value in values {
        validate_plan_relative_markdown_path(value)?;
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
    Ok(())
}

fn apply_single_link_update(
    request: &RefineRewriteRequest,
    change: &MergedLinkChange,
    created_parent_paths: &HashSet<&str>,
    sanitized_payloads: &[(String, String)],
) -> Result<Option<ParentLinkMutation>, RefineRewriteError> {
    let parent_path = request.plan_root.join(&change.parent_relative_path);
    if !parent_path.is_file() {
        return Err(RefineRewriteError::InvalidRewriteScope);
    }

    for child in &change.add_child_relative_paths {
        if !request.plan_root.join(child).is_file() {
            return Err(RefineRewriteError::InvalidRewriteScope);
        }
    }

    let markdown = if path_mutated_before_link_updates(request, &change.parent_relative_path) {
        read_plan_markdown(request, &change.parent_relative_path)?
    } else {
        markdown_from_sanitized_or_disk(request, sanitized_payloads, &change.parent_relative_path)?
    };
    let existing_rows = parse_existing_child_links(&change.parent_relative_path, &markdown)?;
    let existing_paths = existing_rows
        .iter()
        .map(|row| row.canonical_path.as_str())
        .collect::<HashSet<_>>();

    let removed_child_relative_paths = change
        .remove_child_relative_paths
        .iter()
        .filter(|child| existing_paths.contains(child.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let remove_set = removed_child_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut remaining_rows = Vec::new();
    let mut remaining_paths = HashSet::new();
    for row in existing_rows {
        if remove_set.contains(row.canonical_path.as_str()) {
            continue;
        }
        remaining_paths.insert(row.canonical_path.clone());
        remaining_rows.push(row);
    }

    let mut added_child_relative_paths = Vec::new();
    for child in &change.add_child_relative_paths {
        if remaining_paths.contains(child) {
            continue;
        }
        remaining_paths.insert(child.clone());
        added_child_relative_paths.push(child.clone());
    }

    if added_child_relative_paths.is_empty() && removed_child_relative_paths.is_empty() {
        return Ok(None);
    }

    let mut rendered_rows = remaining_rows
        .into_iter()
        .map(|row| row.rendered_line)
        .collect::<Vec<_>>();
    for child in &added_child_relative_paths {
        let title = first_heading_title(&request.plan_root.join(child))?;
        let relative_url = relative_url_from_parent_to_child(&change.parent_relative_path, child)?;
        rendered_rows.push(format!("- [{title}]({relative_url})"));
    }

    let updated = rewrite_child_nodes_section(&markdown, &rendered_rows);
    write_plan_file(&request.plan_root, &change.parent_relative_path, &updated)?;

    Ok(Some(ParentLinkMutation {
        parent_relative_path: change.parent_relative_path.clone(),
        parent_node_id: parent_node_id_for_path(request, &change.parent_relative_path),
        parent_was_created: created_parent_paths.contains(change.parent_relative_path.as_str()),
        added_child_relative_paths,
        removed_child_relative_paths,
    }))
}

fn parse_existing_child_links(
    parent_relative_path: &str,
    markdown: &str,
) -> Result<Vec<ChildLinkRow>, RefineRewriteError> {
    let Some((_, body_start, section_end)) = child_nodes_section_offsets(markdown) else {
        return Ok(Vec::new());
    };
    let body = &markdown[body_start..section_end];
    let mut rows = Vec::new();
    for line in body.lines() {
        let Some((label, url)) = parse_markdown_child_link(line) else {
            continue;
        };
        let Some(canonical_path) = canonical_child_path_from_url(parent_relative_path, &url)?
        else {
            continue;
        };
        rows.push(ChildLinkRow {
            canonical_path,
            rendered_line: format!("- [{label}]({url})"),
        });
    }
    Ok(rows)
}

fn parse_markdown_child_link(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let link_start = trimmed.strip_prefix("- [")?;
    let label_end = link_start.find("](")?;
    let label = &link_start[..label_end];
    let rest = &link_start[label_end + 2..];
    let url_end = rest.find(')')?;
    Some((label.to_owned(), rest[..url_end].to_owned()))
}

fn canonical_child_path_from_url(
    parent_relative_path: &str,
    url: &str,
) -> Result<Option<String>, RefineRewriteError> {
    if url.starts_with('#') || url.contains("://") {
        return Ok(None);
    }
    let url = url
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url);
    let url_path = Path::new(url);
    if url_path.is_absolute() {
        return Err(RefineRewriteError::InvalidRewriteScope);
    }
    let parent_dir = Path::new(parent_relative_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut components = path_components(parent_dir)?;
    for component in url_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_string_lossy().to_string()),
            Component::ParentDir => {
                if components.pop().is_none() {
                    return Err(RefineRewriteError::InvalidRewriteScope);
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(RefineRewriteError::InvalidRewriteScope);
            }
        }
    }
    let canonical = components.join("/");
    validate_plan_relative_markdown_path(&canonical)?;
    Ok(Some(canonical))
}

fn rewrite_child_nodes_section(markdown: &str, rendered_rows: &[String]) -> String {
    let rendered_section = render_child_nodes_section(rendered_rows);
    match child_nodes_section_offsets(markdown) {
        Some((heading_start, _, section_end)) => {
            let mut output = String::with_capacity(markdown.len() + rendered_section.len());
            output.push_str(&markdown[..heading_start]);
            output.push_str(&rendered_section);
            output.push_str(&markdown[section_end..]);
            output
        }
        None => {
            let mut output = String::with_capacity(markdown.len() + rendered_section.len() + 2);
            output.push_str(markdown);
            if !markdown.ends_with('\n') {
                output.push('\n');
            }
            output.push('\n');
            output.push_str(&rendered_section);
            output
        }
    }
}

fn render_child_nodes_section(rendered_rows: &[String]) -> String {
    let mut output = String::from("## Child Nodes\n\n");
    for row in rendered_rows {
        output.push_str(row);
        output.push('\n');
    }
    output
}

fn child_nodes_section_offsets(markdown: &str) -> Option<(usize, usize, usize)> {
    let mut offset = 0;
    let mut heading_start = None;
    let mut body_start = 0;
    for line in markdown.split_inclusive('\n') {
        if heading_start.is_none() && line.trim() == "## Child Nodes" {
            heading_start = Some(offset);
            body_start = offset + line.len();
        } else if heading_start.is_some() && is_next_top_level_section(line) {
            return Some((heading_start.unwrap(), body_start, offset));
        }
        offset += line.len();
    }
    if let Some(heading_start) = heading_start {
        return Some((heading_start, body_start, markdown.len()));
    }
    None
}

fn is_next_top_level_section(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("## ") && trimmed.trim() != "## Child Nodes"
}

fn first_heading_title(path: &Path) -> Result<String, RefineRewriteError> {
    let markdown = fs::read_to_string(path).map_err(|source| RefineRewriteError::Io {
        path: path.display().to_string(),
        source: source.to_string(),
    })?;
    for line in markdown.lines() {
        if let Some(title) = line.trim_start().strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return Ok(title.to_owned());
            }
        }
    }
    Ok(title_from_path(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("node.md"),
    ))
}

fn relative_url_from_parent_to_child(
    parent_relative_path: &str,
    child_relative_path: &str,
) -> Result<String, RefineRewriteError> {
    validate_plan_relative_markdown_path(parent_relative_path)?;
    validate_plan_relative_markdown_path(child_relative_path)?;
    let parent_dir = Path::new(parent_relative_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let parent_parts = path_components(parent_dir)?;
    let child_parts = path_components(Path::new(child_relative_path))?;
    let common_len = parent_parts
        .iter()
        .zip(child_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative_parts = Vec::new();
    for _ in common_len..parent_parts.len() {
        relative_parts.push("..".to_owned());
    }
    relative_parts.extend(child_parts[common_len..].iter().cloned());
    let relative = relative_parts.join("/");
    if relative.starts_with("..") {
        Ok(relative)
    } else {
        Ok(format!("./{relative}"))
    }
}

fn path_components(path: &Path) -> Result<Vec<String>, RefineRewriteError> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_string_lossy().to_string()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RefineRewriteError::InvalidRewriteScope);
            }
        }
    }
    Ok(components)
}

fn parent_node_id_for_path(
    request: &RefineRewriteRequest,
    parent_relative_path: &str,
) -> Option<String> {
    request
        .decisions
        .iter()
        .flat_map(|decision| decision.affected_scope.affected_tracked_nodes.iter())
        .find(|node| node.relative_path.as_deref() == Some(parent_relative_path))
        .map(|node| node.node_id.clone())
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
            if first_markdown_heading_offset(&markdown).is_none() {
                return Err(RefineRewriteError::PostWriteStructureViolation {
                    relative_path: changed.to_owned(),
                    reason: "required plan headings were lost".to_owned(),
                });
            }
        }
    }
    for parent in request
        .rewrite_scope
        .link_changes
        .iter()
        .map(|change| change.parent_relative_path.as_str())
    {
        let path = request.plan_root.join(parent);
        if !path.is_file() {
            continue;
        }
        let markdown = fs::read_to_string(&path).map_err(|source| RefineRewriteError::Io {
            path: path.display().to_string(),
            source: source.to_string(),
        })?;
        for row in parse_existing_child_links(parent, &markdown)? {
            if !request.plan_root.join(&row.canonical_path).is_file() {
                return Err(RefineRewriteError::PostWriteStructureViolation {
                    relative_path: parent.to_owned(),
                    reason: format!("broken child link: {}", row.canonical_path),
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
    let unresolved_follow_ups = request.blocked_follow_ups.clone();
    let summary = build_refine_rewrite_summary(
        &reports.changed_files,
        &reports.structural_changes,
        &reports.stale_nodes,
        &expected_gate_targets,
        &unresolved_follow_ups,
        reports.context_invalidations.len(),
        reports.unchanged_nodes.len(),
    );
    Ok(RefineRewriteResult {
        changed_files: reports.changed_files,
        structural_changes: reports.structural_changes,
        stale_nodes: reports.stale_nodes,
        context_invalidations: reports.context_invalidations,
        unchanged_nodes: reports.unchanged_nodes,
        expected_gate_targets,
        unresolved_follow_ups,
        summary,
    })
}

fn replacement_for_path(request: &RefineRewriteRequest, relative_path: &str) -> Option<String> {
    let mut replacement = None;
    for action in request
        .decisions
        .iter()
        .flat_map(|decision| decision.rewrite_actions.iter())
    {
        if action.target_relative_path.as_deref() == Some(relative_path)
            && let Some(markdown) = &action.replacement_markdown
        {
            replacement = Some(markdown.clone());
        }
    }
    replacement
}

fn sanitized_replacement_for_path(
    request: &RefineRewriteRequest,
    relative_path: &str,
) -> Result<Option<String>, RefineRewriteError> {
    let Some(markdown) = replacement_for_path(request, relative_path) else {
        return Ok(None);
    };
    let preserved_blocks = preserved_comment_blocks_for_path(request, relative_path);
    remove_comment_blocks(relative_path, &markdown, &preserved_blocks).map(Some)
}

fn markdown_from_sanitized_or_disk(
    request: &RefineRewriteRequest,
    sanitized_payloads: &[(String, String)],
    relative_path: &str,
) -> Result<String, RefineRewriteError> {
    if let Some((_, markdown)) = sanitized_payloads
        .iter()
        .find(|(path, _)| path == relative_path)
    {
        return Ok(markdown.clone());
    }
    read_plan_markdown(request, relative_path)
}

fn read_plan_markdown(
    request: &RefineRewriteRequest,
    relative_path: &str,
) -> Result<String, RefineRewriteError> {
    let path = request.plan_root.join(relative_path);
    fs::read_to_string(&path).map_err(|source| RefineRewriteError::Io {
        path: path.display().to_string(),
        source: source.to_string(),
    })
}

fn path_mutated_before_link_updates(request: &RefineRewriteRequest, relative_path: &str) -> bool {
    request
        .rewrite_scope
        .rewrite_targets
        .iter()
        .any(|target| target.relative_path == relative_path)
        || request
            .rewrite_scope
            .node_creations
            .iter()
            .any(|creation| creation.relative_path == relative_path)
        || request
            .rewrite_scope
            .stale_descendants
            .iter()
            .any(|stale| stale.relative_path == relative_path)
}

fn remove_comment_blocks(
    relative_path: &str,
    markdown: &str,
    preserved_blocks: &[PreservedCommentBlock],
) -> Result<String, RefineRewriteError> {
    let parsed_blocks =
        parse_comment_blocks_for_file(relative_path, markdown).map_err(map_comment_error)?;
    let mut output = String::new();
    let mut preserve_current_block = None::<bool>;
    for (index, line) in markdown.split_inclusive('\n').enumerate() {
        let line_number = index + 1;
        match line.trim() {
            "BEGIN_COMMENT" => {
                let preserve = parsed_blocks
                    .iter()
                    .find(|block| block.start_line == line_number)
                    .is_some_and(|block| should_preserve_comment_block(block, preserved_blocks));
                preserve_current_block = Some(preserve);
                if preserve {
                    output.push_str(line);
                }
            }
            "END_COMMENT" => {
                if preserve_current_block.unwrap_or(false) {
                    output.push_str(line);
                }
                preserve_current_block = None;
            }
            _ if preserve_current_block.unwrap_or(true) => output.push_str(line),
            _ => {}
        }
    }
    Ok(output)
}

fn preserved_comment_blocks_for_path(
    request: &RefineRewriteRequest,
    relative_path: &str,
) -> Vec<PreservedCommentBlock> {
    request
        .blocked_follow_ups
        .iter()
        .flat_map(|decision| decision.source_comments.iter())
        .filter(|source| source.source_path == relative_path)
        .map(comment_source_to_preserved_block)
        .collect()
}

fn comment_source_to_preserved_block(source: &RefineCommentSource) -> PreservedCommentBlock {
    PreservedCommentBlock {
        start_line: source.begin_comment_line,
        end_line: source.end_comment_line,
    }
}

fn should_preserve_comment_block(
    block: &CommentBlock,
    preserved_blocks: &[PreservedCommentBlock],
) -> bool {
    preserved_blocks.iter().any(|preserved| {
        preserved.start_line == block.start_line && preserved.end_line == block.end_line
    })
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
    use crate::refine::summary::{RefineStaleNodeSummaryEntry, RefineStaleNodeSummaryKind};
    use crate::refine::{
        RefineAffectedScope, RefineAffectedTrackedNode, RefineCommentSource,
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

    #[test]
    fn refine_rewrite_preserves_unresolved_comment_blocks() {
        let plan_root = temp_plan_root();
        fs::write(
            plan_root.join("api.md"),
            "# API\n\nBEGIN_COMMENT\napply this\nEND_COMMENT\n\nBody\n\nBEGIN_COMMENT\nlater\nEND_COMMENT\n",
        )
        .unwrap();
        let applied = confirmed_decision(
            Vec::new(),
            vec![RefineRewriteAction {
                action_kind: RefineRewriteActionKind::UpdateExistingNode,
                target_relative_path: Some("api.md".to_owned()),
                parent_relative_path: None,
                node_kind: Some("leaf".to_owned()),
                link_change: None,
                replacement_markdown: None,
                rationale: None,
            }],
        );
        let follow_up = RefineDecision {
            source_comments: vec![RefineCommentSource {
                source_path: "api.md".to_owned(),
                begin_comment_line: 9,
                end_comment_line: 11,
                comment_text: Some("later".to_owned()),
            }],
            ..confirmed_decision(Vec::new(), Vec::new())
        };

        let result = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![applied],
            rewrite_scope: RefineRewriteScope {
                rewrite_targets: vec![RefineRewriteTarget {
                    relative_path: "api.md".to_owned(),
                    node_id: Some("leaf-1".to_owned()),
                    action_kind: RefineRewriteActionKind::UpdateExistingNode,
                }],
                ..Default::default()
            },
            blocked_follow_ups: vec![follow_up],
        })
        .expect("rewrite should preserve deferred comments");

        let updated = fs::read_to_string(plan_root.join("api.md")).unwrap();
        assert!(!updated.contains("apply this"));
        assert!(updated.contains("BEGIN_COMMENT\nlater\nEND_COMMENT"));
        assert_eq!(updated.matches("BEGIN_COMMENT").count(), 1);
        assert_eq!(result.unresolved_follow_ups.len(), 1);
    }

    #[test]
    fn refine_non_text_rewrites_remove_processed_comment_blocks() {
        let plan_root = temp_plan_root();
        fs::create_dir_all(plan_root.join("api")).unwrap();
        fs::write(
            plan_root.join("api/api.md"),
            "# API\n\nBEGIN_COMMENT\nremove old child\nEND_COMMENT\n\n## Child Nodes\n\n- [Remove](./remove.md)\n",
        )
        .unwrap();
        fs::write(plan_root.join("api/remove.md"), "# Remove\n").unwrap();
        fs::write(
            plan_root.join("api/stale.md"),
            "# Stale\n\nBEGIN_COMMENT\nstale this node\nEND_COMMENT\n",
        )
        .unwrap();

        apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![confirmed_decision(Vec::new(), Vec::new())],
            rewrite_scope: RefineRewriteScope {
                stale_descendants: vec![RefineStaleDescendant {
                    relative_path: "api/stale.md".to_owned(),
                    node_id: Some("stale-1".to_owned()),
                    reason: "parent changed".to_owned(),
                }],
                link_changes: vec![RefineLinkChange {
                    parent_relative_path: "api/api.md".to_owned(),
                    add_child_relative_paths: Vec::new(),
                    remove_child_relative_paths: vec!["api/remove.md".to_owned()],
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect("link-only and stale-only rewrite should pass");

        let parent_markdown = fs::read_to_string(plan_root.join("api/api.md")).unwrap();
        assert!(!parent_markdown.contains("BEGIN_COMMENT"));
        assert!(!parent_markdown.contains("remove old child"));
        assert!(!parent_markdown.contains("remove.md"));

        let stale_markdown = fs::read_to_string(plan_root.join("api/stale.md")).unwrap();
        assert!(stale_markdown.starts_with("<!-- loopy-refine-status: stale -->"));
        assert!(!stale_markdown.contains("BEGIN_COMMENT"));
        assert!(!stale_markdown.contains("stale this node"));
    }

    #[test]
    fn refine_side_effect_decisions_remove_processed_source_comment_blocks() {
        let plan_root = temp_plan_root();
        fs::write(
            plan_root.join("source.md"),
            "# Source\n\nBEGIN_COMMENT\ncreate the child elsewhere\nEND_COMMENT\n\nBody\n",
        )
        .unwrap();

        let mut decision = confirmed_decision(
            Vec::new(),
            vec![RefineRewriteAction {
                action_kind: RefineRewriteActionKind::CreateNode,
                target_relative_path: Some("created.md".to_owned()),
                parent_relative_path: None,
                node_kind: Some("leaf".to_owned()),
                link_change: None,
                replacement_markdown: Some("# Created\n".to_owned()),
                rationale: None,
            }],
        );
        decision.source_comments = vec![RefineCommentSource {
            source_path: "source.md".to_owned(),
            begin_comment_line: 3,
            end_comment_line: 5,
            comment_text: Some("create the child elsewhere".to_owned()),
        }];

        apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![decision],
            rewrite_scope: RefineRewriteScope {
                node_creations: vec![crate::refine::RefineNodeCreation {
                    relative_path: "created.md".to_owned(),
                    parent_relative_path: None,
                    node_kind: Some("leaf".to_owned()),
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect("side-effect-only rewrite should pass");

        let source_markdown = fs::read_to_string(plan_root.join("source.md")).unwrap();
        assert!(!source_markdown.contains("BEGIN_COMMENT"));
        assert!(!source_markdown.contains("create the child elsewhere"));
        assert_eq!(
            fs::read_to_string(plan_root.join("created.md")).unwrap(),
            "# Created\n"
        );
    }

    #[test]
    fn refine_prevalidates_link_updates_before_file_mutations() {
        let plan_root = temp_plan_root();
        fs::write(plan_root.join("stale.md"), "# Stale\n").unwrap();

        let error = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![confirmed_decision(Vec::new(), Vec::new())],
            rewrite_scope: RefineRewriteScope {
                stale_descendants: vec![RefineStaleDescendant {
                    relative_path: "stale.md".to_owned(),
                    node_id: Some("stale-1".to_owned()),
                    reason: "parent changed".to_owned(),
                }],
                link_changes: vec![RefineLinkChange {
                    parent_relative_path: "api/api.md".to_owned(),
                    add_child_relative_paths: vec!["api/child.md".to_owned()],
                    remove_child_relative_paths: vec!["api/child.md".to_owned()],
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect_err("invalid link update should fail before stale mutation");

        assert_eq!(error, RefineRewriteError::InvalidRewriteScope);
        assert_eq!(
            fs::read_to_string(plan_root.join("stale.md")).unwrap(),
            "# Stale\n"
        );
    }

    #[test]
    fn refine_replacement_markdown_is_sanitized_before_write() {
        let plan_root = temp_plan_root();
        fs::write(plan_root.join("api.md"), "# API\n").unwrap();

        apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![confirmed_decision(
                Vec::new(),
                vec![RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::UpdateExistingNode,
                    target_relative_path: Some("api.md".to_owned()),
                    parent_relative_path: None,
                    node_kind: Some("leaf".to_owned()),
                    link_change: None,
                    replacement_markdown: Some(
                        "# API\n\nBEGIN_COMMENT\nprocessed feedback\nEND_COMMENT\n\nUpdated\n"
                            .to_owned(),
                    ),
                    rationale: None,
                }],
            )],
            rewrite_scope: RefineRewriteScope {
                rewrite_targets: vec![RefineRewriteTarget {
                    relative_path: "api.md".to_owned(),
                    node_id: Some("leaf-1".to_owned()),
                    action_kind: RefineRewriteActionKind::UpdateExistingNode,
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect("replacement rewrite should pass");

        let updated = fs::read_to_string(plan_root.join("api.md")).unwrap();
        assert!(!updated.contains("BEGIN_COMMENT"));
        assert!(!updated.contains("processed feedback"));
        assert!(updated.contains("Updated\n"));
    }

    #[test]
    fn refine_rewrite_uses_later_explicit_replacement_for_duplicate_target_actions() {
        let plan_root = temp_plan_root();
        fs::write(plan_root.join("api.md"), "# API\n\nOriginal\n").unwrap();

        apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![
                confirmed_decision(
                    Vec::new(),
                    vec![RefineRewriteAction {
                        action_kind: RefineRewriteActionKind::UpdateExistingNode,
                        target_relative_path: Some("api.md".to_owned()),
                        parent_relative_path: None,
                        node_kind: Some("leaf".to_owned()),
                        link_change: None,
                        replacement_markdown: None,
                        rationale: Some("first comment only marks the path".to_owned()),
                    }],
                ),
                confirmed_decision(
                    Vec::new(),
                    vec![RefineRewriteAction {
                        action_kind: RefineRewriteActionKind::UpdateExistingNode,
                        target_relative_path: Some("api.md".to_owned()),
                        parent_relative_path: None,
                        node_kind: Some("leaf".to_owned()),
                        link_change: None,
                        replacement_markdown: Some("# API\n\nSecond replacement\n".to_owned()),
                        rationale: Some("later comment provides the merged rewrite".to_owned()),
                    }],
                ),
            ],
            rewrite_scope: RefineRewriteScope {
                rewrite_targets: vec![RefineRewriteTarget {
                    relative_path: "api.md".to_owned(),
                    node_id: Some("leaf-1".to_owned()),
                    action_kind: RefineRewriteActionKind::UpdateExistingNode,
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect("duplicate target rewrite should pass");

        assert_eq!(
            fs::read_to_string(plan_root.join("api.md")).unwrap(),
            "# API\n\nSecond replacement\n"
        );
    }

    #[test]
    fn refine_stale_nodes_are_marked_not_deleted() {
        let plan_root = temp_plan_root();
        fs::write(
            plan_root.join("stale.md"),
            "<!-- loopy-refine-status: active -->\n<!-- loopy-refine-stale-reason: old -->\n# Stale Leaf\n\nBody stays byte-for-byte.\n",
        )
        .unwrap();
        fs::write(plan_root.join("removed.md"), "# Removed\n").unwrap();

        let result = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![confirmed_decision(Vec::new(), Vec::new())],
            rewrite_scope: RefineRewriteScope {
                stale_descendants: vec![
                    RefineStaleDescendant {
                        relative_path: "stale.md".to_owned(),
                        node_id: Some("stale-1".to_owned()),
                        reason: "superseded by parent".to_owned(),
                    },
                    RefineStaleDescendant {
                        relative_path: "stale.md".to_owned(),
                        node_id: Some("stale-1".to_owned()),
                        reason: "final reason with --> marker\nand newline".to_owned(),
                    },
                ],
                node_removals: vec![crate::refine::RefineNodeRemoval {
                    relative_path: "removed.md".to_owned(),
                    parent_relative_path: None,
                    node_id: Some("removed-1".to_owned()),
                    explicit: true,
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect("stale marking should pass");

        let stale_markdown = fs::read_to_string(plan_root.join("stale.md")).unwrap();
        assert!(plan_root.join("stale.md").is_file());
        assert!(!plan_root.join("removed.md").exists());
        assert!(stale_markdown.starts_with(
            "<!-- loopy-refine-status: stale -->\n<!-- loopy-refine-stale-reason: final reason with -- > marker and newline -->\n# Stale Leaf\n"
        ));
        assert_eq!(stale_markdown.matches("loopy-refine-status").count(), 1);
        assert_eq!(
            stale_markdown.matches("loopy-refine-stale-reason").count(),
            1
        );
        assert!(stale_markdown.contains("# Stale Leaf\n\nBody stays byte-for-byte.\n"));
        assert_eq!(
            result
                .changed_files
                .iter()
                .filter(|file| file.change_kind == RefineChangedFileKind::StaleMarked)
                .count(),
            1
        );
        assert!(result.changed_files.iter().any(|file| {
            file.relative_path == "removed.md"
                && file.change_kind == RefineChangedFileKind::ExplicitlyRemoved
        }));
        assert_eq!(
            result.stale_nodes,
            vec![RefineStaleNode {
                relative_path: "stale.md".to_owned(),
                node_id: Some("stale-1".to_owned()),
                reason: "final reason with -- > marker and newline".to_owned(),
            }]
        );
        assert_eq!(
            result.summary.stale_node_summary_entries,
            vec![
                RefineStaleNodeSummaryEntry {
                    relative_path: "removed.md".to_owned(),
                    node_id: Some("removed-1".to_owned()),
                    summary_kind: RefineStaleNodeSummaryKind::ExplicitlyRemoved,
                    reason: None,
                },
                RefineStaleNodeSummaryEntry {
                    relative_path: "stale.md".to_owned(),
                    node_id: Some("stale-1".to_owned()),
                    summary_kind: RefineStaleNodeSummaryKind::StaleMarked,
                    reason: Some("final reason with -- > marker and newline".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn refine_link_updates_own_child_nodes_sections() {
        let plan_root = temp_plan_root();
        fs::create_dir_all(plan_root.join("api/child")).unwrap();
        fs::create_dir_all(plan_root.join("new/child")).unwrap();
        fs::write(
            plan_root.join("api/api.md"),
            "# API\n\nIntro stays.\n\n## Child Nodes\n\n- [Keep](./keep.md)\n- [Remove](./remove.md)\n\n## Notes\n\nDo not edit.\n",
        )
        .unwrap();
        fs::write(plan_root.join("api/keep.md"), "# Keep\n").unwrap();
        fs::write(plan_root.join("api/remove.md"), "# Remove\n").unwrap();
        fs::write(plan_root.join("api/add.md"), "# Add\n").unwrap();
        fs::write(plan_root.join("api/child/child.md"), "# Child\n").unwrap();

        let decision = confirmed_decision(
            vec![RefineAffectedTrackedNode {
                node_id: "parent-1".to_owned(),
                relative_path: Some("api/api.md".to_owned()),
            }],
            vec![
                RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::CreateNode,
                    target_relative_path: Some("new/new.md".to_owned()),
                    parent_relative_path: None,
                    node_kind: Some("parent".to_owned()),
                    link_change: None,
                    replacement_markdown: Some("# New Parent\n\n## Child Nodes\n\n".to_owned()),
                    rationale: None,
                },
                RefineRewriteAction {
                    action_kind: RefineRewriteActionKind::CreateNode,
                    target_relative_path: Some("new/child/child.md".to_owned()),
                    parent_relative_path: Some("new/new.md".to_owned()),
                    node_kind: Some("leaf".to_owned()),
                    link_change: None,
                    replacement_markdown: Some("# New Child\n".to_owned()),
                    rationale: None,
                },
            ],
        );

        let result = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![decision],
            rewrite_scope: RefineRewriteScope {
                node_creations: vec![
                    crate::refine::RefineNodeCreation {
                        relative_path: "new/new.md".to_owned(),
                        parent_relative_path: None,
                        node_kind: Some("parent".to_owned()),
                    },
                    crate::refine::RefineNodeCreation {
                        relative_path: "new/child/child.md".to_owned(),
                        parent_relative_path: Some("new/new.md".to_owned()),
                        node_kind: Some("leaf".to_owned()),
                    },
                ],
                link_changes: vec![
                    crate::refine::RefineLinkChange {
                        parent_relative_path: "api/api.md".to_owned(),
                        add_child_relative_paths: vec![
                            "api/add.md".to_owned(),
                            "api/add.md".to_owned(),
                        ],
                        remove_child_relative_paths: vec![
                            "api/remove.md".to_owned(),
                            "api/missing-removed.md".to_owned(),
                        ],
                    },
                    crate::refine::RefineLinkChange {
                        parent_relative_path: "api/api.md".to_owned(),
                        add_child_relative_paths: vec!["api/child/child.md".to_owned()],
                        remove_child_relative_paths: vec!["api/remove.md".to_owned()],
                    },
                    crate::refine::RefineLinkChange {
                        parent_relative_path: "new/new.md".to_owned(),
                        add_child_relative_paths: vec!["new/child/child.md".to_owned()],
                        remove_child_relative_paths: Vec::new(),
                    },
                ],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect("link updates should pass");

        let api_markdown = fs::read_to_string(plan_root.join("api/api.md")).unwrap();
        assert!(api_markdown.contains("Intro stays."));
        assert!(api_markdown.contains("## Notes\n\nDo not edit."));
        assert!(api_markdown.contains("- [Keep](./keep.md)"));
        assert!(api_markdown.contains("- [Add](./add.md)"));
        assert!(api_markdown.contains("- [Child](./child/child.md)"));
        assert!(!api_markdown.contains("remove.md"));

        let new_parent_markdown = fs::read_to_string(plan_root.join("new/new.md")).unwrap();
        assert!(new_parent_markdown.contains("## Child Nodes\n\n- [New Child](./child/child.md)"));

        assert!(result.changed_files.iter().any(|file| {
            file.relative_path == "api/api.md"
                && file.change_kind == RefineChangedFileKind::TextUpdated
        }));
        assert!(!result.changed_files.iter().any(|file| {
            file.relative_path == "new/new.md"
                && file.change_kind == RefineChangedFileKind::TextUpdated
        }));
        assert_eq!(
            result
                .structural_changes
                .iter()
                .filter(|change| change.parent_relative_path == "api/api.md")
                .map(|change| &change.change_kind)
                .collect::<Vec<_>>(),
            vec![
                &RefineStructuralChangeKind::ChangedChildSet,
                &RefineStructuralChangeKind::ParentContractChanged,
            ]
        );
        let api_change = result
            .structural_changes
            .iter()
            .find(|change| {
                change.parent_relative_path == "api/api.md"
                    && change.change_kind == RefineStructuralChangeKind::ChangedChildSet
            })
            .unwrap();
        assert_eq!(api_change.parent_node_id.as_deref(), Some("parent-1"));
        assert_eq!(
            api_change.added_child_relative_paths,
            vec!["api/add.md", "api/child/child.md"]
        );
        assert_eq!(
            api_change.removed_child_relative_paths,
            vec!["api/remove.md"]
        );

        let conflict_error = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root: plan_root.clone(),
            decisions: vec![confirmed_decision(Vec::new(), Vec::new())],
            rewrite_scope: RefineRewriteScope {
                link_changes: vec![crate::refine::RefineLinkChange {
                    parent_relative_path: "api/api.md".to_owned(),
                    add_child_relative_paths: vec!["api/add.md".to_owned()],
                    remove_child_relative_paths: vec!["api/add.md".to_owned()],
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect_err("same child add/remove should fail");
        assert_eq!(conflict_error, RefineRewriteError::InvalidRewriteScope);

        let missing_add_error = apply_refine_rewrite(RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            decisions: vec![confirmed_decision(Vec::new(), Vec::new())],
            rewrite_scope: RefineRewriteScope {
                link_changes: vec![crate::refine::RefineLinkChange {
                    parent_relative_path: "api/api.md".to_owned(),
                    add_child_relative_paths: vec!["api/does-not-exist.md".to_owned()],
                    remove_child_relative_paths: Vec::new(),
                }],
                ..Default::default()
            },
            blocked_follow_ups: Vec::new(),
        })
        .expect_err("missing add target should fail during link update");
        assert_eq!(missing_add_error, RefineRewriteError::InvalidRewriteScope);
    }

    #[test]
    fn refine_rewrite_summary_is_populated_from_result_buckets() {
        let plan_root = temp_plan_root();
        let follow_up = confirmed_decision(Vec::new(), Vec::new());
        let request = RefineRewriteRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            decisions: vec![
                confirmed_decision(Vec::new(), Vec::new()).with_expected_gates(
                    ExpectedGateRevalidation {
                        leaf_targets: vec!["leaf.md".to_owned()],
                        frontier_targets: vec!["parent/parent.md".to_owned()],
                        reasons: vec!["changed".to_owned()],
                    },
                ),
            ],
            rewrite_scope: RefineRewriteScope::default(),
            blocked_follow_ups: vec![follow_up],
        };
        let reports = MutationReports {
            changed_files: vec![
                RefineChangedFile {
                    relative_path: "leaf.md".to_owned(),
                    node_id: Some("leaf-1".to_owned()),
                    change_kind: RefineChangedFileKind::TextUpdated,
                },
                RefineChangedFile {
                    relative_path: "created.md".to_owned(),
                    node_id: None,
                    change_kind: RefineChangedFileKind::Created,
                },
                RefineChangedFile {
                    relative_path: "removed.md".to_owned(),
                    node_id: Some("removed-1".to_owned()),
                    change_kind: RefineChangedFileKind::ExplicitlyRemoved,
                },
                RefineChangedFile {
                    relative_path: "stale.md".to_owned(),
                    node_id: Some("stale-1".to_owned()),
                    change_kind: RefineChangedFileKind::StaleMarked,
                },
            ],
            structural_changes: vec![RefineStructuralChange {
                parent_relative_path: "parent/parent.md".to_owned(),
                parent_node_id: Some("parent-1".to_owned()),
                change_kind: RefineStructuralChangeKind::ParentContractChanged,
                added_child_relative_paths: vec!["leaf.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
            }],
            stale_nodes: vec![RefineStaleNode {
                relative_path: "stale.md".to_owned(),
                node_id: Some("stale-1".to_owned()),
                reason: "invalidated".to_owned(),
            }],
            context_invalidations: Vec::new(),
            unchanged_nodes: Vec::new(),
        };

        let result = assemble_refine_rewrite_result(&request, reports).unwrap();

        assert_eq!(result.summary.changed_files, result.changed_files);
        assert_eq!(result.summary.structural_changes, result.structural_changes);
        assert_eq!(
            result.summary.stale_node_summary_entries,
            vec![
                RefineStaleNodeSummaryEntry {
                    relative_path: "created.md".to_owned(),
                    node_id: None,
                    summary_kind: RefineStaleNodeSummaryKind::Created,
                    reason: None,
                },
                RefineStaleNodeSummaryEntry {
                    relative_path: "removed.md".to_owned(),
                    node_id: Some("removed-1".to_owned()),
                    summary_kind: RefineStaleNodeSummaryKind::ExplicitlyRemoved,
                    reason: None,
                },
                RefineStaleNodeSummaryEntry {
                    relative_path: "stale.md".to_owned(),
                    node_id: Some("stale-1".to_owned()),
                    summary_kind: RefineStaleNodeSummaryKind::StaleMarked,
                    reason: Some("invalidated".to_owned()),
                },
            ]
        );
        assert_eq!(
            result.summary.expected_gate_targets,
            result.expected_gate_targets
        );
        assert_eq!(
            result.summary.unresolved_follow_ups,
            result.unresolved_follow_ups
        );
        assert_eq!(result.summary.changed_file_count, 4);
        assert_eq!(result.summary.stale_node_count, 3);
        assert_eq!(result.summary.expected_leaf_gate_target_count, 1);
        assert_eq!(result.summary.expected_frontier_gate_target_count, 1);

        let empty = assemble_refine_rewrite_result(
            &RefineRewriteRequest {
                plan_id: "plan-1".to_owned(),
                plan_root: temp_plan_root(),
                decisions: Vec::new(),
                rewrite_scope: RefineRewriteScope::default(),
                blocked_follow_ups: Vec::new(),
            },
            MutationReports::default(),
        )
        .unwrap();
        assert!(empty.summary.changed_files.is_empty());
        assert!(empty.summary.structural_changes.is_empty());
        assert!(empty.summary.stale_node_summary_entries.is_empty());
        assert!(empty.summary.expected_gate_targets.is_empty());
        assert!(empty.summary.unresolved_follow_ups.is_empty());
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

    fn confirmed_decision(
        affected_nodes: Vec<RefineAffectedTrackedNode>,
        rewrite_actions: Vec<RefineRewriteAction>,
    ) -> RefineDecision {
        RefineDecision {
            source_comments: Vec::new(),
            affected_scope: RefineAffectedScope {
                affected_files: Vec::new(),
                affected_tracked_nodes: affected_nodes,
                affected_subtree_roots: Vec::new(),
                unresolved_mapping_note: None,
            },
            change_types: vec![],
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

    trait DecisionTestExt {
        fn with_expected_gates(self, gate: ExpectedGateRevalidation) -> Self;
    }

    impl DecisionTestExt for RefineDecision {
        fn with_expected_gates(mut self, gate: ExpectedGateRevalidation) -> Self {
            self.expected_gate_revalidation.push(gate);
            self
        }
    }
}
