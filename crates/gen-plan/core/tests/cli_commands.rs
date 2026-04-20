mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use loopy_gen_plan::{EnsureNodeIdRequest, EnsurePlanRequest, OpenPlanRequest, Runtime};
use serde_json::Value;

const REAL_LEAF_SUMMARY: &str = "Leaf review passed through fake codex.";

fn cargo_binary() -> Result<std::ffi::OsString> {
    std::env::var_os("CARGO").context("CARGO should be set by cargo test")
}

fn workspace_manifest() -> Result<PathBuf> {
    let output = Command::new(cargo_binary()?)
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .context("failed to locate workspace manifest")?;
    if !output.status.success() {
        bail!(
            "failed to locate workspace manifest:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(PathBuf::from(
        String::from_utf8(output.stdout)
            .context("workspace manifest path must be utf-8")?
            .trim(),
    ))
}

#[test]
fn help_lists_plan_and_gate_commands() -> Result<()> {
    let output = run_cli(&["--help"], None)?;
    if !output.status.success() {
        bail!(
            "expected --help to succeed, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    for subcommand in [
        "ensure-plan",
        "open-plan",
        "ensure-node-id",
        "run-leaf-review-gate",
        "run-frontier-review-gate",
    ] {
        assert!(
            stdout.contains(subcommand),
            "expected help output to contain {subcommand}, stdout was:\n{stdout}"
        );
    }

    Ok(())
}

#[test]
fn ensure_plan_command_prints_pretty_json() -> Result<()> {
    let workspace = support::workspace()?;
    support::assert_dir_exists(workspace.path());

    let output = run_cli(
        &[
            "--workspace",
            workspace
                .path()
                .to_str()
                .context("workspace path must be utf-8")?,
            "ensure-plan",
            "--plan-name",
            "cli-ensure-plan",
            "--task-type",
            "coding-task",
            "--project-directory",
            workspace
                .path()
                .to_str()
                .context("workspace path must be utf-8")?,
        ],
        Some("failed to run loopy-gen-plan ensure-plan"),
    )?;
    if !output.status.success() {
        bail!(
            "expected ensure-plan command to succeed, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("\n  \"plan_id\""),
        "expected pretty-printed JSON output, stdout was:\n{stdout}"
    );
    let value: Value = serde_json::from_str(&stdout)?;
    assert_eq!(value["plan_status"], Value::String("active".to_owned()));

    let runtime = Runtime::new(workspace.path())?;
    let opened = runtime.open_plan(OpenPlanRequest {
        plan_name: "cli-ensure-plan".to_owned(),
    })?;
    assert_eq!(value["plan_id"], Value::String(opened.plan_id));
    assert_eq!(value["plan_root"], Value::String(opened.plan_root));

    Ok(())
}

#[test]
fn open_plan_command_prints_pretty_json() -> Result<()> {
    let workspace = support::workspace()?;
    support::assert_dir_exists(workspace.path());
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "cli-open-plan".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let output = run_cli(
        &[
            "--workspace",
            workspace
                .path()
                .to_str()
                .context("workspace path must be utf-8")?,
            "open-plan",
            "--plan-name",
            "cli-open-plan",
        ],
        Some("failed to run loopy-gen-plan open-plan"),
    )?;
    if !output.status.success() {
        bail!(
            "expected open-plan command to succeed, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("\n  \"plan_id\""),
        "expected pretty-printed JSON output, stdout was:\n{stdout}"
    );
    let value: Value = serde_json::from_str(&stdout)?;
    assert_eq!(value["plan_id"], Value::String(plan.plan_id));
    assert_eq!(value["plan_root"], Value::String(plan.plan_root));
    assert_eq!(value["plan_status"], Value::String(plan.plan_status));
    assert_eq!(value["task_type"], Value::String("coding-task".to_owned()));

    Ok(())
}

#[test]
fn ensure_node_id_command_prints_pretty_json() -> Result<()> {
    let workspace = support::workspace()?;
    support::assert_dir_exists(workspace.path());
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "cli-ensure-node".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;

    let output = run_cli(
        &[
            "--workspace",
            workspace
                .path()
                .to_str()
                .context("workspace path must be utf-8")?,
            "ensure-node-id",
            "--plan-id",
            &plan.plan_id,
            "--relative-path",
            "docs/spec.md",
            "--parent-relative-path",
            "docs",
        ],
        Some("failed to run loopy-gen-plan ensure-node-id"),
    )?;
    if !output.status.success() {
        bail!(
            "expected ensure-node-id command to succeed, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("\n  \"node_id\""),
        "expected pretty-printed JSON output, stdout was:\n{stdout}"
    );
    let value: Value = serde_json::from_str(&stdout)?;
    let reopened = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id,
        relative_path: "docs/spec.md".to_owned(),
        parent_relative_path: Some("docs".to_owned()),
    })?;
    assert_eq!(value["node_id"], Value::String(reopened.node_id));

    Ok(())
}

#[test]
fn leaf_gate_command_prints_pretty_json() -> Result<()> {
    let workspace = support::workspace()?;
    support::assert_dir_exists(workspace.path());
    write_dev_registry(
        workspace.path(),
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../skills/gen-plan")
            .canonicalize()
            .context("checked-in skills/gen-plan root should resolve")?,
    )?;
    let project_directory = workspace.path().join("project");
    fs::create_dir_all(&project_directory)?;
    let fake_bin_dir = workspace.path().join("fake-bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), &project_directory)?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "cli-leaf-gate".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: project_directory.clone(),
    })?;
    let node = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/implement-endpoint.md".to_owned(),
        parent_relative_path: None,
    })?;
    fs::create_dir_all(workspace.path().join(".loopy/plans/cli-leaf-gate/api"))?;
    fs::write(
        workspace
            .path()
            .join(".loopy/plans/cli-leaf-gate/api/implement-endpoint.md"),
        "# Implement endpoint\n",
    )?;

    let output = run_cli_with_env(
        &[
            "--workspace",
            workspace
                .path()
                .to_str()
                .context("workspace path must be utf-8")?,
            "run-leaf-review-gate",
            "--plan-id",
            &plan.plan_id,
            "--node-id",
            &node.node_id,
            "--planner-mode",
            "auto",
        ],
        &[(
            "PATH",
            std::env::join_paths(
                std::iter::once(fake_bin_dir.clone()).chain(
                    std::env::var_os("PATH")
                        .iter()
                        .flat_map(std::env::split_paths),
                ),
            )
            .expect("PATH should remain joinable"),
        )],
        Some("failed to run loopy-gen-plan run-leaf-review-gate"),
    )?;
    if !output.status.success() {
        bail!(
            "expected gate command to succeed, stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("\n  \"gate_run_id\""),
        "expected pretty-printed JSON output, stdout was:\n{stdout}"
    );
    let value: Value = serde_json::from_str(&stdout)?;
    assert_eq!(value["passed"], Value::Bool(true));
    assert_eq!(
        value["verdict"],
        Value::String("approved_as_leaf".to_owned())
    );
    assert_eq!(
        value["summary"],
        Value::String(REAL_LEAF_SUMMARY.to_owned())
    );
    assert_eq!(
        value["reviewer_role_id"],
        Value::String("codex_default".to_owned())
    );

    Ok(())
}

#[test]
fn invalid_planner_mode_is_rejected_for_gate_command() -> Result<()> {
    let output = run_cli(
        &[
            "run-leaf-review-gate",
            "--plan-id",
            "demo-plan",
            "--node-id",
            "demo-node",
            "--planner-mode",
            "bogus",
        ],
        Some("failed to run loopy-gen-plan with invalid planner mode"),
    )?;

    assert!(
        !output.status.success(),
        "expected invalid planner mode to fail, stdout was:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("invalid planner_mode `bogus`: expected `manual` or `auto`"),
        "unexpected stderr:\n{stderr}"
    );

    Ok(())
}

fn run_cli(args: &[&str], context_message: Option<&str>) -> Result<std::process::Output> {
    run_cli_with_env(args, &[], context_message)
}

fn run_cli_with_env(
    args: &[&str],
    envs: &[(&str, std::ffi::OsString)],
    context_message: Option<&str>,
) -> Result<std::process::Output> {
    let manifest_path = workspace_manifest()?;
    let context_message = context_message.unwrap_or("failed to run loopy-gen-plan");
    let mut command = Command::new(cargo_binary()?);
    command
        .args([
            "run",
            "--quiet",
            "--offline",
            "--manifest-path",
            manifest_path
                .to_str()
                .context("workspace manifest path must be utf-8")?,
            "-p",
            "loopy-gen-plan",
            "--",
        ])
        .args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().with_context(|| context_message.to_owned())
}

fn write_dev_registry(
    workspace_root: &std::path::Path,
    gen_plan_skill_root: &std::path::Path,
) -> Result<()> {
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

fn write_fake_codex(
    bin_path: &std::path::Path,
    expected_project_directory: &std::path::Path,
) -> Result<()> {
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

if [[ "$prompt" != *"Gate: leaf_review"* ]]; then
  echo "unexpected prompt payload" >&2
  exit 1
fi

mkdir -p "$(dirname "$output_file")"
cat >"$output_file" <<'EOF'
{{"verdict":"approved_as_leaf","summary":"Leaf review passed through fake codex.","issues":[]}}
EOF
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
