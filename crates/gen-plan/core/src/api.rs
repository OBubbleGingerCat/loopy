use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerMode {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsurePlanRequest {
    pub plan_name: String,
    pub task_type: String,
    pub project_directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsurePlanResponse {
    pub plan_id: String,
    pub plan_root: String,
    pub plan_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPlanRequest {
    pub plan_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPlanResponse {
    pub plan_id: String,
    pub plan_root: String,
    pub plan_status: String,
    pub task_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureNodeIdRequest {
    pub plan_id: String,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureNodeIdResponse {
    pub node_id: String,
}
