mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

#[test]
fn install_script_copies_required_gen_plan_assets() -> Result<()> {
    let workspace = support::workspace()?;
    support::assert_dir_exists(workspace.path());
    let install_root = workspace.path().join("installed-skill");

    let output = Command::new("bash")
        .arg("scripts/install-gen-plan-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root())
        .output()
        .context("failed to run install-gen-plan-skill.sh")?;

    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for relative_path in [
        "SKILL.md",
        "bundle.toml",
        "gen-plan.toml",
        "prompts/domain_contract.md",
        "prompts/leaf_runtime.md",
        "prompts/frontier_runtime.md",
        "roles/coding-task/task-type.toml",
        "roles/coding-task/leaf_reviewer/codex_default.md",
        "roles/coding-task/frontier_reviewer/codex_default.md",
        "bin/loopy-gen-plan",
    ] {
        assert!(
            install_root.join(relative_path).is_file(),
            "expected installed file at {}",
            install_root.join(relative_path).display()
        );
    }

    let bundled_binary = install_root.join("bin/loopy-gen-plan");
    let mode = fs::metadata(&bundled_binary)?.permissions().mode();
    assert_ne!(mode & 0o111, 0, "expected bundled binary to be executable");

    Ok(())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root should resolve")
}
