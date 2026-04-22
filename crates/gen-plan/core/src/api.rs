use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlannerMode {
    Manual,
    Auto,
}

impl PlannerMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Parent,
    Leaf,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Leaf => "leaf",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnsurePlanRequest {
    pub plan_name: String,
    pub task_type: String,
    pub project_directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnsurePlanResponse {
    pub plan_id: String,
    pub plan_root: String,
    pub plan_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenPlanRequest {
    pub plan_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenPlanResponse {
    pub plan_id: String,
    pub plan_root: String,
    pub plan_status: String,
    pub task_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnsureNodeIdRequest {
    pub plan_id: String,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnsureNodeIdResponse {
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectNodeRequest {
    pub plan_id: String,
    pub node_id: Option<String>,
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeSummary {
    pub node_id: String,
    pub relative_path: String,
    pub node_kind: NodeKind,
    pub parent_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateSummary {
    pub gate_run_id: String,
    pub reviewer_role_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectNodeResponse {
    pub node_id: String,
    pub relative_path: String,
    pub node_kind: NodeKind,
    pub parent_node_id: Option<String>,
    pub parent_relative_path: Option<String>,
    pub children: Vec<NodeSummary>,
    pub latest_passed_leaf_gate_summary: Option<GateSummary>,
    pub latest_frontier_gate_summary: Option<GateSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListChildrenRequest {
    pub plan_id: String,
    pub parent_node_id: Option<String>,
    pub parent_relative_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListChildrenResponse {
    pub parent_node_id: String,
    pub parent_relative_path: String,
    pub children: Vec<NodeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewIssue {
    pub issue_kind: String,
    pub target_node_id: Option<String>,
    pub target_parent_node_id: Option<String>,
    pub target_node_ids: Option<Vec<String>>,
    pub summary: String,
    pub rationale: String,
    pub expected_revision: String,
    pub question_for_user: Option<String>,
    pub decision_impact: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunLeafReviewGateRequest {
    pub plan_id: String,
    pub node_id: String,
    pub planner_mode: PlannerMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunLeafReviewGateResponse {
    pub gate_run_id: String,
    pub passed: bool,
    pub verdict: String,
    pub summary: String,
    pub reviewer_role_id: String,
    pub issues: Vec<ReviewIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunFrontierReviewGateRequest {
    pub plan_id: String,
    pub parent_node_id: String,
    pub planner_mode: PlannerMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunFrontierReviewGateResponse {
    pub gate_run_id: String,
    pub passed: bool,
    pub verdict: String,
    pub summary: String,
    pub reviewer_role_id: String,
    pub issues: Vec<ReviewIssue>,
    pub invalidated_leaf_node_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::EnsurePlanRequest;

    #[test]
    fn ensure_plan_request_keeps_project_directory_as_pathbuf() {
        let request: EnsurePlanRequest = serde_json::from_value(json!({
            "plan_name": "demo-plan",
            "task_type": "coding-task",
            "project_directory": "/tmp/project",
        }))
        .expect("request should deserialize");

        assert_eq!(
            std::any::type_name_of_val(&request.project_directory),
            std::any::type_name::<PathBuf>()
        );
        assert_eq!(request.project_directory, PathBuf::from("/tmp/project"));
    }
}
