mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use loopy_gen_plan::{
    EnsureNodeIdRequest, EnsurePlanRequest, PlannerMode, RunFrontierReviewGateRequest,
    RunLeafReviewGateRequest, Runtime,
};
use rusqlite::{params, Connection};

const REAL_LEAF_SUMMARY: &str = "Leaf review passed through fake codex.";
const REAL_FRONTIER_SUMMARY: &str = "Frontier review passed through fake codex.";
const FRONTIER_LEAF_CHILD_NODE_ID: &str = "node-frontier-leaf-child";
const FRONTIER_NON_LEAF_CHILD_NODE_ID: &str = "node-frontier-non-leaf-child";
const FRONTIER_GRANDCHILD_NODE_ID: &str = "node-frontier-grandchild";

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
fn leaf_gate_dispatches_real_reviewer_and_persists_selected_role_id() -> Result<()> {
    let workspace = support::workspace()?;
    write_dev_registry(
        workspace.path(),
        &repo_root().join("skills").join("gen-plan"),
    )?;
    let project_directory = workspace.path().join("project");
    fs::create_dir_all(&project_directory)?;
    let fake_bin_dir = workspace.path().join("fake-bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), &project_directory)?;

    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "leaf-gate".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: project_directory.clone(),
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

    let result = {
        let _env_guard = fake_codex_env(&fake_bin_dir);
        runtime.run_leaf_review_gate(RunLeafReviewGateRequest {
            plan_id: plan.plan_id,
            node_id: node.node_id,
            planner_mode: PlannerMode::Auto,
        })?
    };

    assert!(result.passed);
    assert!(!result.gate_run_id.is_empty());
    assert_eq!(result.verdict, "approved_as_leaf");
    assert_eq!(result.summary, REAL_LEAF_SUMMARY);
    assert_eq!(result.reviewer_role_id, "codex_default");
    assert!(result.issues.is_empty());

    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    let (persisted_reviewer_role_id, persisted_summary): (String, String) = connection.query_row(
        "SELECT reviewer_role_id, summary
         FROM GEN_PLAN__leaf_gate_runs
         WHERE leaf_gate_run_id = ?1",
        params![result.gate_run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(persisted_reviewer_role_id, "codex_default");
    assert_eq!(persisted_summary, REAL_LEAF_SUMMARY);

    Ok(())
}

#[test]
fn leaf_gate_rejects_non_leaf_nodes() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "leaf-non-leaf-gate".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let target = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/implement-endpoint.md".to_owned(),
        parent_relative_path: None,
    })?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id,
            node_id,
            relative_path,
            node_name,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &plan.plan_id,
            "node-leaf-gate-child",
            "api/implement-endpoint/details.md",
            "details.md",
            &target.node_id,
            "0",
            "0",
        ],
    )?;

    let error = runtime
        .run_leaf_review_gate(RunLeafReviewGateRequest {
            plan_id: plan.plan_id.clone(),
            node_id: target.node_id.clone(),
            planner_mode: PlannerMode::Auto,
        })
        .expect_err("non-leaf node should be rejected for leaf review");
    assert!(
        format!("{error:#}").contains("leaf review"),
        "unexpected error: {error:#}"
    );

    let persisted_run_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM GEN_PLAN__leaf_gate_runs
         WHERE plan_id = ?1 AND node_id = ?2",
        params![&plan.plan_id, &target.node_id],
        |row| row.get(0),
    )?;
    assert_eq!(persisted_run_count, 0);

    Ok(())
}

#[test]
fn frontier_gate_dispatches_real_reviewer_and_persists_selected_role_id() -> Result<()> {
    let workspace = support::workspace()?;
    write_dev_registry(
        workspace.path(),
        &repo_root().join("skills").join("gen-plan"),
    )?;
    let project_directory = workspace.path().join("project");
    fs::create_dir_all(&project_directory)?;
    let fake_bin_dir = workspace.path().join("fake-bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), &project_directory)?;

    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "frontier-gate".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: project_directory.clone(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/frontier-gate");
    fs::create_dir_all(plan_root.join("backend/subtree"))?;
    fs::write(plan_root.join("backend/backend.md"), "# Backend\n")?;
    fs::write(
        plan_root.join("backend/implement-endpoint.md"),
        "# Implement endpoint\n",
    )?;
    fs::write(plan_root.join("backend/subtree.md"), "# Subtree\n")?;
    fs::write(plan_root.join("backend/subtree/details.md"), "# Details\n")?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "backend/backend.md".to_owned(),
        parent_relative_path: None,
    })?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id,
            node_id,
            relative_path,
            node_name,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &plan.plan_id,
            FRONTIER_LEAF_CHILD_NODE_ID,
            "backend/implement-endpoint.md",
            "implement-endpoint.md",
            &parent.node_id,
            "0",
            "0",
        ],
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id,
            node_id,
            relative_path,
            node_name,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &plan.plan_id,
            FRONTIER_NON_LEAF_CHILD_NODE_ID,
            "backend/subtree.md",
            "subtree.md",
            &parent.node_id,
            "0",
            "0",
        ],
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id,
            node_id,
            relative_path,
            node_name,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &plan.plan_id,
            FRONTIER_GRANDCHILD_NODE_ID,
            "backend/subtree/details.md",
            "details.md",
            FRONTIER_NON_LEAF_CHILD_NODE_ID,
            "0",
            "0",
        ],
    )?;

    let result = {
        let _env_guard = fake_codex_env(&fake_bin_dir);
        runtime.run_frontier_review_gate(RunFrontierReviewGateRequest {
            plan_id: plan.plan_id.clone(),
            parent_node_id: parent.node_id.clone(),
            planner_mode: PlannerMode::Auto,
        })?
    };

    assert!(result.passed);
    assert!(!result.gate_run_id.is_empty());
    assert_eq!(result.verdict, "approved_frontier");
    assert_eq!(result.summary, REAL_FRONTIER_SUMMARY);
    assert_eq!(result.reviewer_role_id, "codex_default");
    assert!(result.invalidated_leaf_node_ids.is_empty());
    assert!(result.issues.is_empty());
    let existing_invalidated_leaf_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM GEN_PLAN__nodes
         WHERE plan_id = ?1 AND parent_node_id = ?2 AND node_id = ?3",
        params![&plan.plan_id, &parent.node_id, FRONTIER_LEAF_CHILD_NODE_ID],
        |row| row.get(0),
    )?;
    assert_eq!(existing_invalidated_leaf_count, 1);
    assert!(!result
        .invalidated_leaf_node_ids
        .contains(&FRONTIER_NON_LEAF_CHILD_NODE_ID.to_owned()));

    let (persisted_reviewer_role_id, persisted_summary): (String, String) = connection.query_row(
        "SELECT reviewer_role_id, summary
         FROM GEN_PLAN__frontier_gate_runs
         WHERE frontier_gate_run_id = ?1",
        params![result.gate_run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(persisted_reviewer_role_id, "codex_default");
    assert_eq!(persisted_summary, REAL_FRONTIER_SUMMARY);

    Ok(())
}

#[test]
fn bootstrap_migrates_old_gate_tables_to_add_summary_and_project_directory_columns() -> Result<()> {
    let workspace = support::workspace()?;
    let loopy_dir = workspace.path().join(".loopy");
    create_old_schema_db(&loopy_dir)?;

    let runtime = Runtime::new(workspace.path())?;
    let _ = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "migration-check".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    assert_migrated_columns_present_once(&loopy_dir)?;

    Ok(())
}

#[test]
fn bootstrap_repeatedly_migrates_old_gate_tables_without_duplicate_columns() -> Result<()> {
    let workspace = support::workspace()?;
    let loopy_dir = workspace.path().join(".loopy");
    create_old_schema_db(&loopy_dir)?;

    let first_runtime = Runtime::new(workspace.path())?;
    let _ = first_runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "migration-check-first".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    assert_migrated_columns_present_once(&loopy_dir)?;

    let second_runtime = Runtime::new(workspace.path())?;
    let _ = second_runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "migration-check-second".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    assert_migrated_columns_present_once(&loopy_dir)?;

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

fn create_old_schema_db(loopy_dir: &Path) -> Result<()> {
    fs::create_dir_all(loopy_dir)?;
    let connection = Connection::open(loopy_dir.join("loopy.db"))?;
    connection.execute_batch(
        r#"
        CREATE TABLE GEN_PLAN__plans (
            plan_id TEXT PRIMARY KEY,
            workspace_root TEXT NOT NULL,
            plan_name TEXT NOT NULL,
            plan_root TEXT NOT NULL,
            task_type TEXT NOT NULL,
            plan_status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(workspace_root, plan_name)
        );

        CREATE TABLE GEN_PLAN__nodes (
            plan_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            node_name TEXT NOT NULL,
            parent_node_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(plan_id, node_id),
            UNIQUE(plan_id, relative_path)
        );

        CREATE TABLE GEN_PLAN__leaf_gate_runs (
            leaf_gate_run_id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            planner_mode TEXT NOT NULL,
            reviewer_role_id TEXT NOT NULL,
            passed INTEGER NOT NULL,
            verdict TEXT NOT NULL,
            issues_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE GEN_PLAN__frontier_gate_runs (
            frontier_gate_run_id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            parent_node_id TEXT NOT NULL,
            planner_mode TEXT NOT NULL,
            reviewer_role_id TEXT NOT NULL,
            passed INTEGER NOT NULL,
            verdict TEXT NOT NULL,
            issues_json TEXT NOT NULL,
            invalidated_leaf_node_ids_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn assert_migrated_columns_present_once(loopy_dir: &Path) -> Result<()> {
    let connection = Connection::open(loopy_dir.join("loopy.db"))?;
    let plan_has_project_directory: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM pragma_table_info('GEN_PLAN__plans')
         WHERE name = 'project_directory'",
        [],
        |row| row.get(0),
    )?;
    let leaf_has_summary: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM pragma_table_info('GEN_PLAN__leaf_gate_runs')
         WHERE name = 'summary'",
        [],
        |row| row.get(0),
    )?;
    let frontier_has_summary: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM pragma_table_info('GEN_PLAN__frontier_gate_runs')
         WHERE name = 'summary'",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(plan_has_project_directory, 1);
    assert_eq!(leaf_has_summary, 1);
    assert_eq!(frontier_has_summary, 1);

    Ok(())
}

fn write_fake_codex(bin_path: &Path, expected_project_directory: &Path) -> Result<()> {
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

expected_project_directory="{expected_project_directory}"
workspace_arg=""
output_file=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    exec)
      shift
      ;;
    -C)
      workspace_arg="$2"
      shift 2
      ;;
    -o|--output-last-message)
      output_file="$2"
      shift 2
      ;;
    -c|-m|--color|--add-dir)
      shift 2
      ;;
    --full-auto|--skip-git-repo-check)
      shift
      ;;
    -)
      shift
      ;;
    *)
      shift
      ;;
  esac
done

prompt="$(cat)"

if [[ "$workspace_arg" != "$expected_project_directory" ]]; then
  echo "expected -C $expected_project_directory but saw $workspace_arg" >&2
  exit 1
fi

if [[ "$PWD" != "$expected_project_directory" ]]; then
  echo "expected cwd $expected_project_directory but saw $PWD" >&2
  exit 1
fi

mkdir -p "$(dirname "$output_file")"

if [[ "$prompt" == *"Gate: leaf_review"* ]]; then
  cat >"$output_file" <<'EOF'
{{"verdict":"approved_as_leaf","summary":"Leaf review passed through fake codex.","issues":[]}}
EOF
elif [[ "$prompt" == *"Gate: frontier_review"* ]]; then
  cat >"$output_file" <<'EOF'
{{"verdict":"approved_frontier","summary":"Frontier review passed through fake codex.","issues":[],"invalidated_leaf_node_ids":[]}}
EOF
else
  echo "unexpected prompt payload" >&2
  exit 1
fi

echo "stdout is not the machine-readable result"
"#,
        expected_project_directory = expected_project_directory.display(),
    );
    fs::write(bin_path, script)?;
    let mut permissions = fs::metadata(bin_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(bin_path, permissions)?;
    Ok(())
}

fn fake_codex_env(fake_bin_dir: &Path) -> FakeCodexEnvGuard {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let env_lock = ENV_LOCK.get_or_init(|| Mutex::new(()));
    let guard = env_lock.lock().expect("env lock should not be poisoned");
    let original_path = std::env::var_os("PATH");
    let mut path_entries = vec![fake_bin_dir.to_path_buf()];
    if let Some(existing) = original_path.as_ref() {
        path_entries.extend(std::env::split_paths(existing));
    }
    let updated_path = std::env::join_paths(path_entries).expect("PATH should remain joinable");
    unsafe {
        std::env::set_var("PATH", &updated_path);
    }
    FakeCodexEnvGuard {
        _guard: guard,
        original_path,
    }
}

struct FakeCodexEnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original_path: Option<std::ffi::OsString>,
}

impl Drop for FakeCodexEnvGuard {
    fn drop(&mut self) {
        if let Some(path) = self.original_path.as_ref() {
            unsafe {
                std::env::set_var("PATH", path);
            }
        } else {
            unsafe {
                std::env::remove_var("PATH");
            }
        }
    }
}
