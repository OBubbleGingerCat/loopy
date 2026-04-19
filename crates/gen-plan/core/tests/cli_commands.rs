use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .expect("workspace root should exist")
        .to_path_buf()
}

#[test]
fn help_lists_plan_and_gate_commands() -> Result<()> {
    let output = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "--offline",
            "-p",
            "loopy-gen-plan",
            "--",
            "--help",
        ])
        .current_dir(workspace_root())
        .output()
        .context("failed to run loopy-gen-plan --help")?;
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
