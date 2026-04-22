use std::path::PathBuf;
use std::{fs, path::Path};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use loopy_gen_plan::{
    EnsureNodeIdRequest, EnsurePlanRequest, InspectNodeRequest, ListChildrenRequest,
    OpenPlanRequest, PlannerMode,
    RunFrontierReviewGateRequest, RunLeafReviewGateRequest, Runtime,
};

#[derive(Debug, Parser)]
#[command(name = "loopy-gen-plan")]
#[command(about = "Local runtime for loopy:gen-plan")]
struct Cli {
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    EnsurePlan {
        #[arg(long)]
        plan_name: String,
        #[arg(long)]
        task_type: String,
        #[arg(long)]
        project_directory: PathBuf,
    },
    OpenPlan {
        #[arg(long)]
        plan_name: String,
    },
    EnsureNodeId {
        #[arg(long)]
        plan_id: String,
        #[arg(long)]
        relative_path: String,
        #[arg(long)]
        parent_relative_path: Option<String>,
    },
    InspectNode {
        #[arg(long)]
        plan_id: String,
        #[arg(long)]
        node_id: Option<String>,
        #[arg(long)]
        relative_path: Option<String>,
    },
    ListChildren {
        #[arg(long)]
        plan_id: String,
        #[arg(long)]
        parent_node_id: Option<String>,
        #[arg(long)]
        parent_relative_path: Option<String>,
    },
    RunLeafReviewGate {
        #[arg(long)]
        plan_id: String,
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        planner_mode: String,
    },
    RunFrontierReviewGate {
        #[arg(long)]
        plan_id: String,
        #[arg(long)]
        parent_node_id: String,
        #[arg(long)]
        planner_mode: String,
    },
    MockLeafReviewer {
        #[arg(long = "output-last-message")]
        output_last_message: PathBuf,
        invocation_payload_path: PathBuf,
    },
    MockFrontierReviewer {
        #[arg(long = "output-last-message")]
        output_last_message: PathBuf,
        invocation_payload_path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::EnsurePlan {
            plan_name,
            task_type,
            project_directory,
        } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.ensure_plan(EnsurePlanRequest {
                plan_name,
                task_type,
                project_directory,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::OpenPlan { plan_name } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.open_plan(OpenPlanRequest { plan_name })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::EnsureNodeId {
            plan_id,
            relative_path,
            parent_relative_path,
        } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.ensure_node_id(EnsureNodeIdRequest {
                plan_id,
                relative_path,
                parent_relative_path,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::InspectNode {
            plan_id,
            node_id,
            relative_path,
        } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.inspect_node(InspectNodeRequest {
                plan_id,
                node_id,
                relative_path,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::ListChildren {
            plan_id,
            parent_node_id,
            parent_relative_path,
        } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.list_children(ListChildrenRequest {
                plan_id,
                parent_node_id,
                parent_relative_path,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::RunLeafReviewGate {
            plan_id,
            node_id,
            planner_mode,
        } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.run_leaf_review_gate(RunLeafReviewGateRequest {
                plan_id,
                node_id,
                planner_mode: parse_planner_mode(&planner_mode)?,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::RunFrontierReviewGate {
            plan_id,
            parent_node_id,
            planner_mode,
        } => {
            let workspace = cli.workspace.clone().unwrap_or(std::env::current_dir()?);
            let runtime = Runtime::new(workspace)?;
            let response = runtime.run_frontier_review_gate(RunFrontierReviewGateRequest {
                plan_id,
                parent_node_id,
                planner_mode: parse_planner_mode(&planner_mode)?,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::MockLeafReviewer {
            output_last_message,
            invocation_payload_path,
        } => write_mock_leaf_reviewer_output(&invocation_payload_path, &output_last_message)?,
        Commands::MockFrontierReviewer {
            output_last_message,
            invocation_payload_path,
        } => write_mock_frontier_reviewer_output(&invocation_payload_path, &output_last_message)?,
    }

    Ok(())
}

fn parse_planner_mode(value: &str) -> Result<PlannerMode> {
    match value {
        "manual" => Ok(PlannerMode::Manual),
        "auto" => Ok(PlannerMode::Auto),
        _ => bail!("invalid planner_mode `{value}`: expected `manual` or `auto`"),
    }
}

fn write_mock_leaf_reviewer_output(
    invocation_payload_path: &Path,
    output_last_message_path: &Path,
) -> Result<()> {
    let payload = fs::read_to_string(invocation_payload_path)?;
    let target_node_id = extract_prompt_value(&payload, "- Target Node ID:")?;
    write_last_message_json(
        output_last_message_path,
        &serde_json::json!({
            "verdict": "revise_leaf",
            "summary": "Mock leaf reviewer requires a deterministic revision.",
            "issues": [{
                "issue_kind": "mock_reviewer_revision",
                "target_node_id": target_node_id,
                "target_parent_node_id": null,
                "target_node_ids": null,
                "summary": "Mock leaf reviewer requests a revision.",
                "rationale": "The checked-in mock reviewer always returns a deterministic non-pass result.",
                "expected_revision": "Revise the leaf plan before re-running review.",
                "question_for_user": null,
                "decision_impact": null
            }]
        }),
    )
}

fn write_mock_frontier_reviewer_output(
    invocation_payload_path: &Path,
    output_last_message_path: &Path,
) -> Result<()> {
    let payload = fs::read_to_string(invocation_payload_path)?;
    let target_parent_node_id = extract_prompt_value(&payload, "- Parent Node ID:")?;
    write_last_message_json(
        output_last_message_path,
        &serde_json::json!({
            "verdict": "revise_frontier",
            "summary": "Mock frontier reviewer requires a deterministic revision.",
            "issues": [{
                "issue_kind": "mock_reviewer_revision",
                "target_node_id": null,
                "target_parent_node_id": target_parent_node_id,
                "target_node_ids": null,
                "summary": "Mock frontier reviewer requests a revision.",
                "rationale": "The checked-in mock reviewer always returns a deterministic non-pass result.",
                "expected_revision": "Revise the frontier plan before re-running review.",
                "question_for_user": null,
                "decision_impact": null
            }],
            "invalidated_leaf_node_ids": []
        }),
    )
}

fn extract_prompt_value(payload: &str, prefix: &str) -> Result<String> {
    payload
        .lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("invocation payload missing required line `{prefix}`"))
}

fn write_last_message_json(
    output_last_message_path: &Path,
    value: &serde_json::Value,
) -> Result<()> {
    if let Some(parent) = output_last_message_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output_last_message_path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
