use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use loopy_gen_plan::{EnsureNodeIdRequest, EnsurePlanRequest, OpenPlanRequest, Runtime};

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
    RunLeafReviewGate {},
    RunFrontierReviewGate {},
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
            let _ = (
                runtime,
                EnsurePlanRequest {
                    plan_name,
                    task_type,
                    project_directory,
                },
            );
            println!("ensure-plan not implemented yet");
        }
        Commands::OpenPlan { plan_name } => {
            let _ = (runtime, OpenPlanRequest { plan_name });
            println!("open-plan not implemented yet");
        }
        Commands::EnsureNodeId {
            plan_id,
            relative_path,
            parent_relative_path,
        } => {
            let _ = (
                runtime,
                EnsureNodeIdRequest {
                    plan_id,
                    relative_path,
                    parent_relative_path,
                },
            );
            println!("ensure-node-id not implemented yet");
        }
        Commands::RunLeafReviewGate {} => {
            let _ = runtime;
            println!("run-leaf-review-gate not implemented yet");
        }
        Commands::RunFrontierReviewGate {} => {
            let _ = runtime;
            println!("run-frontier-review-gate not implemented yet");
        }
    }

    Ok(())
}
