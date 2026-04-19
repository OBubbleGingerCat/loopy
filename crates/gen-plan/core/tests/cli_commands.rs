use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

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
    let manifest_path = workspace_manifest()?;
    let output = Command::new(cargo_binary()?)
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
            "--help",
        ])
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
