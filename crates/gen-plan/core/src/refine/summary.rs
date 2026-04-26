use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefineRewriteSummary {
    pub changed_file_count: usize,
    pub structural_change_count: usize,
    pub stale_node_count: usize,
    pub context_invalidation_count: usize,
    pub unchanged_node_count: usize,
    pub expected_leaf_gate_target_count: usize,
    pub expected_frontier_gate_target_count: usize,
    pub unresolved_follow_up_count: usize,
}
