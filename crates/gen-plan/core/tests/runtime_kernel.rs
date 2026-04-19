mod support;

use anyhow::Result;
use loopy_gen_plan::{EnsureNodeIdRequest, EnsurePlanRequest, OpenPlanRequest, Runtime};

#[test]
fn ensure_plan_creates_fixed_plan_root_and_persists_metadata() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;

    let ensured = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let expected_plan_root = workspace.path().join(".loopy/plans/demo-plan");
    support::assert_dir_exists(&expected_plan_root);
    assert_eq!(ensured.plan_root, expected_plan_root.display().to_string());
    assert_eq!(ensured.plan_status, "active");

    let reopened = runtime.open_plan(OpenPlanRequest {
        plan_name: "demo-plan".to_owned(),
    })?;

    assert_eq!(reopened.plan_id, ensured.plan_id);
    assert_eq!(reopened.plan_root, ensured.plan_root);
    assert_eq!(reopened.plan_status, ensured.plan_status);

    Ok(())
}

#[test]
fn ensure_node_id_is_stable_for_a_relative_path() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let first = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/plan.md".to_owned(),
        parent_relative_path: None,
    })?;
    let second = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id,
        relative_path: "docs/plan.md".to_owned(),
        parent_relative_path: None,
    })?;

    assert_eq!(first.node_id, second.node_id);

    Ok(())
}
