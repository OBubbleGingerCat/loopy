use std::path::PathBuf;

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
    pub project_directory: PathBuf,
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
