use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use loopy_gen_plan::{
    EnsureNodeIdRequest, EnsurePlanRequest, OpenPlanRequest, PlannerMode,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let workspace = cli.workspace.unwrap_or(std::env::current_dir()?);
    let runtime = Runtime::new(workspace)?;

    match cli.command {
        Commands::EnsurePlan {
            plan_name,
            task_type,
            project_directory,
        } => {
            let response = runtime.ensure_plan(EnsurePlanRequest {
                plan_name,
                task_type,
                project_directory,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::OpenPlan { plan_name } => {
            let response = runtime.open_plan(OpenPlanRequest { plan_name })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::EnsureNodeId {
            plan_id,
            relative_path,
            parent_relative_path,
        } => {
            let response = runtime.ensure_node_id(EnsureNodeIdRequest {
                plan_id,
                relative_path,
                parent_relative_path,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::RunLeafReviewGate {
            plan_id,
            node_id,
            planner_mode,
        } => {
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
            let response = runtime.run_frontier_review_gate(RunFrontierReviewGateRequest {
                plan_id,
                parent_node_id,
                planner_mode: parse_planner_mode(&planner_mode)?,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
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
