mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use loopy_gen_plan::{EnsurePlanRequest, Runtime};

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
