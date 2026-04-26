use serde::{Deserialize, Serialize};

use super::decision::{ExpectedGateRevalidation, RefineDecision};
use super::rewrite::{
    RefineChangedFile, RefineChangedFileKind, RefineStaleNode, RefineStructuralChange,
};

/// Rewrite Result Summary.
///
/// Ordered slots:
/// 1. changed file summary
/// 2. structural change summary
/// 3. stale node summary
/// 4. expected gate target summary
/// 5. unresolved follow-up summary
///
/// The rewrite result summary precedes actual runtime gate results.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineRewriteSummary {
    pub changed_files: Vec<RefineChangedFile>,
    pub structural_changes: Vec<RefineStructuralChange>,
    pub stale_node_summary_entries: Vec<RefineStaleNodeSummaryEntry>,
    pub expected_gate_targets: Vec<ExpectedGateRevalidation>,
    pub unresolved_follow_ups: Vec<RefineDecision>,
    pub changed_file_count: usize,
    pub structural_change_count: usize,
    pub stale_node_count: usize,
    pub context_invalidation_count: usize,
    pub unchanged_node_count: usize,
    pub expected_leaf_gate_target_count: usize,
    pub expected_frontier_gate_target_count: usize,
    pub unresolved_follow_up_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineStaleNodeSummaryEntry {
    pub relative_path: String,
    pub node_id: Option<String>,
    pub summary_kind: RefineStaleNodeSummaryKind,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineStaleNodeSummaryKind {
    Created,
    ExplicitlyRemoved,
    StaleMarked,
}

pub(crate) fn build_refine_rewrite_summary(
    changed_files: &[RefineChangedFile],
    structural_changes: &[RefineStructuralChange],
    stale_nodes: &[RefineStaleNode],
    expected_gate_targets: &[ExpectedGateRevalidation],
    unresolved_follow_ups: &[RefineDecision],
    context_invalidation_count: usize,
    unchanged_node_count: usize,
) -> RefineRewriteSummary {
    // Summary population consumes final rewrite result buckets and must not infer additional rewrite effects.
    // Changed file summary rules: updated markdown files, created markdown files, and removed markdown files
    // are listed with plan-relative paths.
    // Structural change summary rules: structural changes separately from text-only changes, without
    // duplicating the concrete created removed or stale node list, using the refine change type taxonomy.
    // Stale node summary rules: created explicitly removed and stale nodes or subtrees are visible;
    // stale descendants are reported as marked stale rather than removed, stale descendants are distinguished
    // from active rewritten nodes, and stale-node summary follows the revision invalidation rule.
    // Stale-node summary entries come from stale_nodes plus changed_files entries with Created,
    // ExplicitlyRemoved, or StaleMarked, preserving final stable result order without duplication.
    // Expected gate target summary rules: expected leaf gate revalidation targets and expected frontier gate
    // revalidation targets are listed; expected gate targets are not actual gate results.
    // Unresolved follow-up summary rules: unresolved user-owned decisions and missing prerequisite information
    // are listed, and unresolved follow-ups are not reported as applied changes.
    // The summary excludes actual runtime gate pass/fail results.
    let stale_node_summary_entries = build_stale_node_summary_entries(changed_files, stale_nodes);
    RefineRewriteSummary {
        changed_files: changed_files.to_vec(),
        structural_changes: structural_changes.to_vec(),
        stale_node_summary_entries: stale_node_summary_entries.clone(),
        expected_gate_targets: expected_gate_targets.to_vec(),
        unresolved_follow_ups: unresolved_follow_ups.to_vec(),
        changed_file_count: changed_files.len(),
        structural_change_count: structural_changes.len(),
        stale_node_count: stale_node_summary_entries.len(),
        context_invalidation_count,
        unchanged_node_count,
        expected_leaf_gate_target_count: expected_gate_targets
            .iter()
            .map(|target| target.leaf_targets.len())
            .sum(),
        expected_frontier_gate_target_count: expected_gate_targets
            .iter()
            .map(|target| target.frontier_targets.len())
            .sum(),
        unresolved_follow_up_count: unresolved_follow_ups.len(),
    }
}

fn build_stale_node_summary_entries(
    changed_files: &[RefineChangedFile],
    stale_nodes: &[RefineStaleNode],
) -> Vec<RefineStaleNodeSummaryEntry> {
    let mut entries = Vec::new();
    for changed_file in changed_files {
        let summary_kind = match changed_file.change_kind {
            RefineChangedFileKind::Created => RefineStaleNodeSummaryKind::Created,
            RefineChangedFileKind::ExplicitlyRemoved => {
                RefineStaleNodeSummaryKind::ExplicitlyRemoved
            }
            RefineChangedFileKind::StaleMarked => RefineStaleNodeSummaryKind::StaleMarked,
            RefineChangedFileKind::TextUpdated | RefineChangedFileKind::Regenerated => continue,
        };
        let stale_node = stale_nodes
            .iter()
            .find(|node| node.relative_path == changed_file.relative_path);
        entries.push(RefineStaleNodeSummaryEntry {
            relative_path: changed_file.relative_path.clone(),
            node_id: changed_file
                .node_id
                .clone()
                .or_else(|| stale_node.and_then(|node| node.node_id.clone())),
            summary_kind,
            reason: stale_node.map(|node| node.reason.clone()),
        });
    }

    for stale_node in stale_nodes {
        if entries
            .iter()
            .any(|entry| entry.relative_path == stale_node.relative_path)
        {
            continue;
        }
        entries.push(RefineStaleNodeSummaryEntry {
            relative_path: stale_node.relative_path.clone(),
            node_id: stale_node.node_id.clone(),
            summary_kind: RefineStaleNodeSummaryKind::StaleMarked,
            reason: Some(stale_node.reason.clone()),
        });
    }

    entries
}
