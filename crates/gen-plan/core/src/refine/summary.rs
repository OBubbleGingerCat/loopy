use serde::{Deserialize, Serialize};

use super::decision::{ExpectedGateRevalidation, RefineDecision};
use super::rewrite::{RefineChangedFile, RefineStaleNode, RefineStructuralChange};

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
    pub stale_nodes: Vec<RefineStaleNode>,
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
    // Expected gate target summary rules: expected leaf gate revalidation targets and expected frontier gate
    // revalidation targets are listed; expected gate targets are not actual gate results.
    // Unresolved follow-up summary rules: unresolved user-owned decisions and missing prerequisite information
    // are listed, and unresolved follow-ups are not reported as applied changes.
    // The summary excludes actual runtime gate pass/fail results.
    RefineRewriteSummary {
        changed_files: changed_files.to_vec(),
        structural_changes: structural_changes.to_vec(),
        stale_nodes: stale_nodes.to_vec(),
        expected_gate_targets: expected_gate_targets.to_vec(),
        unresolved_follow_ups: unresolved_follow_ups.to_vec(),
        changed_file_count: changed_files.len(),
        structural_change_count: structural_changes.len(),
        stale_node_count: stale_nodes.len(),
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
