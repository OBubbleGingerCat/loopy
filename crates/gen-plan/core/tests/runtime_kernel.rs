mod support;

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use loopy_gen_plan::{
    EnsureNodeIdRequest, EnsurePlanRequest, InspectNodeRequest, ListChildrenRequest, NodeKind,
    OpenPlanRequest, ReconcileParentChildLinksRequest, Runtime,
};
use rusqlite::{Connection, params};

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
fn refine_entry_opens_existing_plan_metadata() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let project_directory = workspace.path().join("project");
    fs::create_dir_all(&project_directory)?;

    let ensured = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "refine-entry".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory,
    })?;

    let reopened = runtime.open_plan(OpenPlanRequest {
        plan_name: "refine-entry".to_owned(),
    })?;

    assert_eq!(reopened.plan_id, ensured.plan_id);
    assert_eq!(reopened.plan_root, ensured.plan_root);
    assert_eq!(reopened.plan_status, "active");
    assert_eq!(reopened.task_type, "coding-task");
    assert_eq!(
        reopened.project_directory,
        workspace.path().join("project").display().to_string()
    );

    Ok(())
}

#[test]
fn refine_entry_rejects_missing_plan_target() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let missing_root = workspace.path().join(".loopy/plans/missing-refine");

    let error = runtime
        .open_plan(OpenPlanRequest {
            plan_name: "missing-refine".to_owned(),
        })
        .expect_err("missing refine target should not be created by open_plan");

    assert!(
        format!("{error:#}").contains("does not exist"),
        "unexpected missing-target error: {error:#}"
    );
    assert!(
        !missing_root.exists(),
        "open_plan must not fall back to ensure_plan for refine targets"
    );

    Ok(())
}

#[test]
fn refine_entry_rejects_invalid_plan_name() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;

    for plan_name in [
        "/tmp/outside-plan",
        "../outside-plan",
        "nested/plan",
        "nested/../../escape",
    ] {
        let error = runtime
            .open_plan(OpenPlanRequest {
                plan_name: plan_name.to_owned(),
            })
            .expect_err("invalid refine plan_name should be rejected");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains("plan_name"),
            "unexpected error for `{plan_name}`: {error_text}"
        );
    }

    Ok(())
}

#[test]
fn ensure_plan_repairs_existing_project_directory_for_reopened_plan() -> Result<()> {
    let workspace = support::workspace()?;
    let loopy_dir = workspace.path().join(".loopy");
    fs::create_dir_all(&loopy_dir)?;
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
    connection.execute(
        "INSERT INTO GEN_PLAN__plans (
            plan_id,
            workspace_root,
            plan_name,
            plan_root,
            task_type,
            plan_status,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "plan-existing",
            workspace.path().display().to_string(),
            "demo-plan",
            workspace
                .path()
                .join(".loopy/plans/demo-plan")
                .display()
                .to_string(),
            "coding-task",
            "active",
            "0",
            "0",
        ],
    )?;

    let corrected_project_directory = workspace.path().join("project");
    let runtime = Runtime::new(workspace.path())?;
    let ensured = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: corrected_project_directory.clone(),
    })?;

    assert_eq!(ensured.plan_id, "plan-existing");
    let (persisted_project_directory, persisted_project_directory_source): (String, String) =
        Connection::open(loopy_dir.join("loopy.db"))?.query_row(
            "SELECT project_directory, project_directory_source
             FROM GEN_PLAN__plans
             WHERE plan_id = ?1",
            params!["plan-existing"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    assert_eq!(
        persisted_project_directory,
        corrected_project_directory.display().to_string()
    );
    assert_eq!(persisted_project_directory_source, "explicit");

    Ok(())
}

#[test]
fn bootstrap_backfills_legacy_root_plan_node_kind_as_parent() -> Result<()> {
    let workspace = support::workspace()?;
    let loopy_dir = workspace.path().join(".loopy");
    fs::create_dir_all(&loopy_dir)?;
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
        "#,
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__plans (
            plan_id,
            workspace_root,
            plan_name,
            plan_root,
            task_type,
            plan_status,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "plan-existing",
            workspace.path().display().to_string(),
            "demo",
            workspace
                .path()
                .join(".loopy/plans/demo")
                .display()
                .to_string(),
            "coding-task",
            "active",
            "0",
            "0",
        ],
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "plan-existing",
            "root-1",
            "demo.md",
            "demo",
            Option::<String>::None,
            "0",
            "0",
        ],
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let root = runtime.inspect_node(InspectNodeRequest {
        plan_id: "plan-existing".to_owned(),
        node_id: None,
        relative_path: Some("demo.md".to_owned()),
    })?;
    assert_eq!(root.node_kind, NodeKind::Parent);
    assert_eq!(root.parent_relative_path, None);

    let persisted_node_kind: String = Connection::open(loopy_dir.join("loopy.db"))?.query_row(
        "SELECT node_kind
         FROM GEN_PLAN__nodes
         WHERE plan_id = ?1 AND node_id = ?2",
        params!["plan-existing", "root-1"],
        |row| row.get(0),
    )?;
    assert_eq!(persisted_node_kind, "parent");

    Ok(())
}

#[test]
fn bootstrap_reclassifies_legacy_root_plan_node_kind_stored_as_leaf() -> Result<()> {
    let workspace = support::workspace()?;
    let loopy_dir = workspace.path().join(".loopy");
    fs::create_dir_all(&loopy_dir)?;
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
            node_kind TEXT NOT NULL,
            parent_node_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(plan_id, node_id),
            UNIQUE(plan_id, relative_path)
        );
        "#,
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__plans (
            plan_id,
            workspace_root,
            plan_name,
            plan_root,
            task_type,
            plan_status,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "plan-existing",
            workspace.path().display().to_string(),
            "demo",
            workspace
                .path()
                .join(".loopy/plans/demo")
                .display()
                .to_string(),
            "coding-task",
            "active",
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
            node_kind,
            parent_node_id,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "plan-existing",
            "root-1",
            "demo.md",
            "demo",
            "leaf",
            Option::<String>::None,
            "0",
            "0",
        ],
    )?;

    let runtime = Runtime::new(workspace.path())?;
    let ensured = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: "plan-existing".to_owned(),
        relative_path: "demo.md".to_owned(),
        parent_relative_path: None,
    })?;
    assert_eq!(ensured.node_id, "root-1");

    let root = runtime.inspect_node(InspectNodeRequest {
        plan_id: "plan-existing".to_owned(),
        node_id: Some("root-1".to_owned()),
        relative_path: None,
    })?;
    assert_eq!(root.node_kind, NodeKind::Parent);
    assert_eq!(root.parent_relative_path, None);

    let persisted_node_kind: String = Connection::open(loopy_dir.join("loopy.db"))?.query_row(
        "SELECT node_kind
         FROM GEN_PLAN__nodes
         WHERE plan_id = ?1 AND node_id = ?2",
        params!["plan-existing", "root-1"],
        |row| row.get(0),
    )?;
    assert_eq!(persisted_node_kind, "parent");

    Ok(())
}

#[test]
fn ensure_plan_rejects_project_directory_redirect_for_existing_non_legacy_plan() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let original_project_directory = workspace.path().join("proj-a");
    let redirected_project_directory = workspace.path().join("proj-b");
    fs::create_dir_all(&original_project_directory)?;
    fs::create_dir_all(&redirected_project_directory)?;

    let ensured = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: original_project_directory.clone(),
    })?;

    let error = runtime
        .ensure_plan(EnsurePlanRequest {
            plan_name: "demo-plan".to_owned(),
            task_type: "coding-task".to_owned(),
            project_directory: redirected_project_directory,
        })
        .expect_err("non-legacy project_directory mismatch should be rejected");
    assert!(
        format!("{error:#}").contains("project_directory"),
        "unexpected error: {error:#}"
    );

    let persisted_project_directory: String =
        Connection::open(workspace.path().join(".loopy/loopy.db"))?.query_row(
            "SELECT project_directory FROM GEN_PLAN__plans WHERE plan_id = ?1",
            params![ensured.plan_id],
            |row| row.get(0),
        )?;
    assert_eq!(
        persisted_project_directory,
        original_project_directory.display().to_string()
    );

    Ok(())
}

#[test]
fn ensure_plan_normalizes_equivalent_project_directory_spellings() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    fs::create_dir_all(workspace.path().join("project"))?;

    let first = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "project-dir-spelling".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: PathBuf::from("./project"),
    })?;
    let second = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "project-dir-spelling".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: PathBuf::from("project"),
    })?;

    assert_eq!(second.plan_id, first.plan_id);
    let persisted_project_directory: String =
        Connection::open(workspace.path().join(".loopy/loopy.db"))?.query_row(
            "SELECT project_directory FROM GEN_PLAN__plans WHERE plan_id = ?1",
            params![first.plan_id],
            |row| row.get(0),
        )?;
    assert_eq!(
        persisted_project_directory,
        workspace.path().join("project").display().to_string()
    );

    Ok(())
}

#[test]
fn ensure_plan_does_not_treat_explicit_workspace_root_as_legacy_project_directory() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let redirected_project_directory = workspace.path().join("proj-b");
    fs::create_dir_all(&redirected_project_directory)?;

    let ensured = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let error = runtime
        .ensure_plan(EnsurePlanRequest {
            plan_name: "demo-plan".to_owned(),
            task_type: "coding-task".to_owned(),
            project_directory: redirected_project_directory,
        })
        .expect_err("explicit workspace-root project_directory should not be treated as legacy");
    assert!(
        format!("{error:#}").contains("project_directory"),
        "unexpected error: {error:#}"
    );

    let (persisted_project_directory, persisted_project_directory_source): (String, String) =
        Connection::open(workspace.path().join(".loopy/loopy.db"))?.query_row(
            "SELECT project_directory, project_directory_source
             FROM GEN_PLAN__plans
             WHERE plan_id = ?1",
            params![ensured.plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    assert_eq!(
        persisted_project_directory,
        workspace.path().display().to_string()
    );
    assert_eq!(persisted_project_directory_source, "explicit");

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

    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    let first = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    let second = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id,
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    assert_eq!(first.node_id, second.node_id);

    Ok(())
}

#[test]
fn ensure_plan_rejects_plan_names_that_escape_fixed_plan_root() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;

    for plan_name in [
        "/tmp/outside-plan",
        "../outside-plan",
        "nested/plan",
        "nested/../../escape",
    ] {
        let error = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: plan_name.to_owned(),
                task_type: "coding-task".to_owned(),
                project_directory: workspace.path().to_path_buf(),
            })
            .expect_err("invalid plan_name should be rejected");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains("plan_name"),
            "unexpected error for `{plan_name}`: {error_text}"
        );
    }

    Ok(())
}

#[test]
fn ensure_plan_rejects_invalid_task_types() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;

    for task_type in [
        "",
        " ",
        "../coding-task",
        "nested/plan",
        "coding task",
        "coding_task",
        "-coding-task",
        "coding-task-",
        "Coding-task",
        ".coding-task",
    ] {
        let error = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: "demo-plan".to_owned(),
                task_type: task_type.to_owned(),
                project_directory: workspace.path().to_path_buf(),
            })
            .expect_err("invalid task_type should be rejected");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains("task_type"),
            "unexpected error for `{task_type}`: {error_text}"
        );
    }

    Ok(())
}

#[test]
fn ensure_node_id_rejects_paths_that_are_not_valid_plan_local_paths() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    for relative_path in ["/tmp/plan.md", "../plan.md", "./plan.md", "docs/../plan.md"] {
        let error = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: relative_path.to_owned(),
                parent_relative_path: None,
            })
            .expect_err("invalid relative_path should be rejected");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains("relative_path"),
            "unexpected error for `{relative_path}`: {error_text}"
        );
    }

    let error = runtime
        .ensure_node_id(EnsureNodeIdRequest {
            plan_id: plan.plan_id,
            relative_path: "docs/spec.md".to_owned(),
            parent_relative_path: Some("../".to_owned()),
        })
        .expect_err("invalid parent_relative_path should be rejected");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("parent_relative_path"),
        "unexpected parent path error: {error_text}"
    );

    Ok(())
}

#[test]
fn ensure_node_id_accepts_canonical_parent_and_child_paths() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    let leaf = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    let nested_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id,
        relative_path: "docs/cli/cli.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    assert_ne!(parent.node_id, leaf.node_id);
    assert_ne!(leaf.node_id, nested_parent.node_id);

    Ok(())
}

#[test]
fn ensure_node_id_accepts_root_plan_parent_for_scoped_children() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![
            plan.plan_id.as_str(),
            "root-1",
            "demo.md",
            "demo",
            "parent",
            Option::<String>::None
        ],
    )?;

    let leaf = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "demo/leaf.md".to_owned(),
        parent_relative_path: Some("demo.md".to_owned()),
    })?;
    let nested_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "demo/api/api.md".to_owned(),
        parent_relative_path: Some("demo.md".to_owned()),
    })?;

    let leaf = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id.clone(),
        node_id: Some(leaf.node_id),
        relative_path: None,
    })?;
    assert_eq!(leaf.parent_relative_path.as_deref(), Some("demo.md"));
    let nested_parent = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: Some(nested_parent.node_id),
        relative_path: None,
    })?;
    assert_eq!(nested_parent.node_kind, NodeKind::Parent);
    assert_eq!(
        nested_parent.parent_relative_path.as_deref(),
        Some("demo.md")
    );

    Ok(())
}

#[test]
fn ensure_node_id_rejects_sibling_leaf_under_root_plan_parent() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "root-parent-sibling".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "root-parent-sibling.md".to_owned(),
        parent_relative_path: None,
    })?;
    let error = runtime
        .ensure_node_id(EnsureNodeIdRequest {
            plan_id: plan.plan_id,
            relative_path: "guide.md".to_owned(),
            parent_relative_path: Some("root-parent-sibling.md".to_owned()),
        })
        .expect_err("root plan parent must not accept top-level sibling leaves");
    assert!(
        format!("{error:#}").contains("direct child"),
        "unexpected error: {error:#}"
    );

    Ok(())
}

#[test]
fn ensure_node_id_records_plan_root_markdown_as_parent() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let root = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "demo.md".to_owned(),
        parent_relative_path: None,
    })?;
    let inspected = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id.clone(),
        node_id: Some(root.node_id.clone()),
        relative_path: None,
    })?;
    assert_eq!(inspected.relative_path, "demo.md");
    assert_eq!(inspected.node_kind, NodeKind::Parent);
    assert_eq!(inspected.parent_relative_path, None);

    let reopened = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id,
        relative_path: "demo.md".to_owned(),
        parent_relative_path: None,
    })?;
    assert_eq!(reopened.node_id, root.node_id);

    Ok(())
}

#[test]
fn ensure_node_id_rejects_noncanonical_node_shapes_and_missing_parents() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    for relative_path in ["docs", "docs/spec", "docs/spec.md"] {
        let error = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: relative_path.to_owned(),
                parent_relative_path: None,
            })
            .expect_err("noncanonical path shape should be rejected");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains("canonical") || error_text.contains("parent_relative_path"),
            "unexpected error for `{relative_path}`: {error_text}"
        );
    }

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    assert!(!parent.node_id.is_empty());

    let error = runtime
        .ensure_node_id(EnsureNodeIdRequest {
            plan_id: plan.plan_id.clone(),
            relative_path: "docs/cli/notes.md".to_owned(),
            parent_relative_path: Some("docs/docs.md".to_owned()),
        })
        .expect_err("non-direct child path should be rejected");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("direct child") || error_text.contains("canonical"),
        "unexpected non-direct-child error: {error_text}"
    );

    let error = runtime
        .ensure_node_id(EnsureNodeIdRequest {
            plan_id: plan.plan_id,
            relative_path: "missing/spec.md".to_owned(),
            parent_relative_path: Some("missing/missing.md".to_owned()),
        })
        .expect_err("missing tracked parent should be rejected");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("existing tracked parent") || error_text.contains("does not exist"),
        "unexpected missing-parent error: {error_text}"
    );

    Ok(())
}

#[test]
fn ensure_node_id_rejects_conflicting_parent_for_existing_node_with_context() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    let legacy_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/legacy/legacy.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    let node = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "UPDATE GEN_PLAN__nodes
         SET parent_node_id = ?1
         WHERE plan_id = ?2 AND node_id = ?3",
        params![legacy_parent.node_id, &plan.plan_id, &node.node_id],
    )?;

    let error = runtime
        .ensure_node_id(EnsureNodeIdRequest {
            plan_id: plan.plan_id,
            relative_path: "docs/spec.md".to_owned(),
            parent_relative_path: Some("docs/docs.md".to_owned()),
        })
        .expect_err("conflicting parent linkage should be rejected");
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains(&node.node_id),
        "error should include existing node id: {error_text}"
    );
    assert!(
        error_text.contains("docs/docs.md") && error_text.contains("docs/legacy/legacy.md"),
        "error should include both parent paths: {error_text}"
    );

    Ok(())
}

#[test]
fn inspect_node_reports_runtime_metadata_and_direct_children() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "inspect-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    let leaf = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    let nested_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/cli/cli.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    let inspected = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id.clone(),
        node_id: None,
        relative_path: Some("docs/docs.md".to_owned()),
    })?;

    assert_eq!(inspected.node_id, parent.node_id);
    assert_eq!(inspected.relative_path, "docs/docs.md");
    assert_eq!(inspected.node_kind, NodeKind::Parent);
    assert_eq!(inspected.parent_relative_path, None);
    assert_eq!(inspected.children.len(), 2);
    assert_eq!(inspected.children[0].node_id, nested_parent.node_id);
    assert_eq!(inspected.children[0].node_kind, NodeKind::Parent);
    assert_eq!(inspected.children[1].node_id, leaf.node_id);
    assert_eq!(inspected.children[1].node_kind, NodeKind::Leaf);

    let by_id = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: Some(leaf.node_id.clone()),
        relative_path: None,
    })?;
    assert_eq!(by_id.node_id, leaf.node_id);
    assert_eq!(by_id.relative_path, "docs/spec.md");
    assert_eq!(by_id.node_kind, NodeKind::Leaf);
    assert_eq!(by_id.parent_node_id, Some(parent.node_id));

    Ok(())
}

#[test]
fn list_children_can_lookup_by_parent_id_and_path() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "children-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/cli/cli.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    let by_path = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: None,
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    assert_eq!(by_path.parent_node_id, parent.node_id);
    assert_eq!(by_path.parent_relative_path, "docs/docs.md");
    assert_eq!(by_path.children.len(), 2);
    assert_eq!(by_path.children[0].relative_path, "docs/cli/cli.md");
    assert_eq!(by_path.children[1].relative_path, "docs/spec.md");

    let by_id = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id,
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(by_id.children.len(), 2);

    Ok(())
}

#[test]
fn list_children_accepts_root_plan_parent_relative_path() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "root-children".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "root-children.md".to_owned(),
        parent_relative_path: None,
    })?;
    let child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "root-children/leaf.md".to_owned(),
        parent_relative_path: Some("root-children.md".to_owned()),
    })?;

    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id,
        parent_node_id: None,
        parent_relative_path: Some("root-children.md".to_owned()),
    })?;
    assert_eq!(children.parent_node_id, parent.node_id);
    assert_eq!(children.parent_relative_path, "root-children.md");
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].node_id, child.node_id);
    assert_eq!(children.children[0].relative_path, "root-children/leaf.md");

    Ok(())
}

#[test]
fn refine_reuses_tracked_node_identity() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "refine-identity".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    let leaf = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    let reopened = runtime.open_plan(OpenPlanRequest {
        plan_name: "refine-identity".to_owned(),
    })?;
    assert_eq!(reopened.plan_id, plan.plan_id);

    let inspected_parent = runtime.inspect_node(InspectNodeRequest {
        plan_id: reopened.plan_id.clone(),
        node_id: Some(parent.node_id.clone()),
        relative_path: None,
    })?;
    assert_eq!(inspected_parent.node_id, parent.node_id);
    assert_eq!(inspected_parent.relative_path, "docs/docs.md");
    assert_eq!(inspected_parent.children.len(), 1);
    assert_eq!(inspected_parent.children[0].node_id, leaf.node_id);
    assert_eq!(inspected_parent.children[0].relative_path, "docs/spec.md");

    let children_by_id = runtime.list_children(ListChildrenRequest {
        plan_id: reopened.plan_id.clone(),
        parent_node_id: Some(parent.node_id.clone()),
        parent_relative_path: None,
    })?;
    let children_by_path = runtime.list_children(ListChildrenRequest {
        plan_id: reopened.plan_id.clone(),
        parent_node_id: None,
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    assert_eq!(children_by_id.children, children_by_path.children);
    assert_eq!(children_by_id.children[0].node_id, leaf.node_id);

    let plan_root = workspace.path().join(".loopy/plans/refine-identity");
    fs::create_dir_all(plan_root.join("docs"))?;
    fs::write(plan_root.join("docs/unregistered.md"), "# Unregistered\n")?;
    let after_filesystem_only = runtime.list_children(ListChildrenRequest {
        plan_id: reopened.plan_id.clone(),
        parent_node_id: Some(parent.node_id.clone()),
        parent_relative_path: None,
    })?;
    assert_eq!(after_filesystem_only.children.len(), 1);
    let error = runtime
        .inspect_node(InspectNodeRequest {
            plan_id: reopened.plan_id.clone(),
            node_id: None,
            relative_path: Some("docs/unregistered.md".to_owned()),
        })
        .expect_err("filesystem-only markdown must not have runtime identity");
    assert!(
        format!("{error:#}").contains("does not exist"),
        "unexpected untracked-node error: {error:#}"
    );

    let unregistered = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: reopened.plan_id.clone(),
        relative_path: "docs/unregistered.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;
    let after_registration = runtime.list_children(ListChildrenRequest {
        plan_id: reopened.plan_id,
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(after_registration.children.len(), 2);
    assert!(
        after_registration
            .children
            .iter()
            .any(|child| child.node_id == unregistered.node_id)
    );

    Ok(())
}

#[test]
fn reconcile_allows_reparent_when_existing_parent_markdown_was_deleted() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "deleted-source-reparent".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace
        .path()
        .join(".loopy/plans/deleted-source-reparent");
    fs::create_dir_all(plan_root.join("deleted-source-reparent"))?;
    fs::write(
        plan_root.join("deleted-source-reparent.md"),
        "# Root\n\n## Child Nodes\n\n- [Leaf](./deleted-source-reparent/leaf.md)\n",
    )?;
    fs::write(
        plan_root.join("deleted-source-reparent/leaf.md"),
        "# Leaf\n",
    )?;

    let root_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "deleted-source-reparent.md".to_owned(),
        parent_relative_path: None,
    })?;
    let old_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "deleted-source-reparent/deleted-source-reparent.md".to_owned(),
        parent_relative_path: None,
    })?;
    let child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "deleted-source-reparent/leaf.md".to_owned(),
        parent_relative_path: Some("deleted-source-reparent/deleted-source-reparent.md".to_owned()),
    })?;

    let result = runtime.reconcile_parent_child_links(ReconcileParentChildLinksRequest {
        plan_id: plan.plan_id.clone(),
        parent_relative_path: "deleted-source-reparent.md".to_owned(),
    })?;
    assert_eq!(result.parent_node_id, root_parent.node_id);
    assert_eq!(
        result.attached_child_relative_paths,
        vec!["deleted-source-reparent/leaf.md".to_owned()]
    );

    let inspected = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: Some(child.node_id),
        relative_path: None,
    })?;
    assert_eq!(inspected.parent_node_id, Some(root_parent.node_id));
    assert_eq!(
        inspected.parent_relative_path.as_deref(),
        Some("deleted-source-reparent.md")
    );
    assert_ne!(inspected.parent_node_id, Some(old_parent.node_id));

    Ok(())
}

#[test]
fn persisted_plan_root_mismatch_is_rejected_on_readback() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "UPDATE GEN_PLAN__plans SET plan_root = ?1 WHERE plan_id = ?2",
        params!["/tmp/tampered-plan-root", plan.plan_id],
    )?;

    let ensure_error = runtime
        .ensure_plan(EnsurePlanRequest {
            plan_name: "demo-plan".to_owned(),
            task_type: "coding-task".to_owned(),
            project_directory: workspace.path().to_path_buf(),
        })
        .expect_err("mismatched persisted plan_root should be rejected by ensure_plan");
    assert!(
        format!("{ensure_error:#}").contains("plan_root"),
        "unexpected ensure_plan error: {ensure_error:#}"
    );

    let open_error = runtime
        .open_plan(OpenPlanRequest {
            plan_name: "demo-plan".to_owned(),
        })
        .expect_err("mismatched persisted plan_root should be rejected by open_plan");
    assert!(
        format!("{open_error:#}").contains("plan_root"),
        "unexpected open_plan error: {open_error:#}"
    );

    Ok(())
}
