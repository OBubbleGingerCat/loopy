mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use loopy_gen_plan::{
    EnsureNodeIdRequest, EnsurePlanRequest, PlannerMode, RunFrontierReviewGateRequest,
    RunLeafReviewGateRequest, Runtime,
};

#[test]
fn ensure_plan_resolves_default_leaf_and_frontier_reviewers() -> Result<()> {
    let workspace = support::workspace()?;
    write_dev_registry(
        workspace.path(),
        &repo_root().join("skills").join("gen-plan"),
    )?;
    support::assert_dir_exists(&workspace.path().join("skills"));

    let runtime = Runtime::new(workspace.path())?;
    let created = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let resolved = runtime.resolve_gate_roles(&created.plan_id)?;

    assert_eq!(resolved.task_type, "coding-task");
    assert_eq!(resolved.leaf_reviewer_role_id, "codex_default");
    assert_eq!(resolved.frontier_reviewer_role_id, "codex_default");

    Ok(())
}

#[test]
fn leaf_gate_persists_a_failed_mock_review() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "leaf-gate".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let leaf_path = workspace
        .path()
        .join(".loopy/plans/leaf-gate/api/implement-endpoint.md");
    fs::create_dir_all(
        leaf_path
            .parent()
            .expect("leaf path should include a parent directory"),
    )?;
    fs::write(&leaf_path, "# Implement endpoint\n")?;

    let node = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/implement-endpoint.md".to_owned(),
        parent_relative_path: None,
    })?;

    let result = runtime.run_leaf_review_gate(RunLeafReviewGateRequest {
        plan_id: plan.plan_id,
        node_id: node.node_id,
        planner_mode: PlannerMode::Auto,
    })?;

    assert!(!result.passed);
    assert_eq!(result.verdict, "revise_leaf");
    assert_eq!(result.issues.len(), 1);

    Ok(())
}

#[test]
fn frontier_gate_returns_invalidated_leaf_ids() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "frontier-gate".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "backend/backend.md".to_owned(),
        parent_relative_path: None,
    })?;

    let result = runtime.run_frontier_review_gate(RunFrontierReviewGateRequest {
        plan_id: plan.plan_id,
        parent_node_id: parent.node_id,
        planner_mode: PlannerMode::Auto,
    })?;

    assert!(!result.passed);
    assert_eq!(result.verdict, "revise_frontier");
    assert_eq!(
        result.invalidated_leaf_node_ids,
        vec!["node-leaf-1".to_owned()]
    );

    Ok(())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root should resolve")
}

fn write_dev_registry(workspace_root: &Path, gen_plan_skill_root: &Path) -> Result<()> {
    let registry_dir = workspace_root.join("skills");
    fs::create_dir_all(&registry_dir)?;
    fs::write(
        registry_dir.join("dev-registry.toml"),
        format!(
            r#"[[skills]]
skill_id = "loopy:gen-plan"
loader_id = "loopy.gen-plan.v1"
source_root = "{}"
binary_package = "loopy-gen-plan"
binary_name = "loopy-gen-plan"
internal_manifest = "gen-plan.toml"
"#,
            gen_plan_skill_root.display()
        ),
    )?;
    Ok(())
}
