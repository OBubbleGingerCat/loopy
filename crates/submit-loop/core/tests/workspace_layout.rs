mod support;

use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[test]
fn cargo_metadata_reports_a_virtual_workspace_with_members_under_crates() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let root_manifest = repo_root.join("Cargo.toml");

    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root)
        .output()
        .context("failed to run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata did not return a packages array")?;
    assert!(
        !packages.is_empty(),
        "workspace should expose at least one member package"
    );

    let root_manifest_str = root_manifest.display().to_string();
    assert!(
        packages
            .iter()
            .all(|package| package["manifest_path"].as_str() != Some(root_manifest_str.as_str())),
        "root Cargo.toml should be a virtual workspace, not a concrete runtime package: {root_manifest_str}"
    );
    assert!(
        packages.iter().all(|package| {
            package["manifest_path"]
                .as_str()
                .map(|path| path.contains("/crates/"))
                .unwrap_or(false)
        }),
        "all workspace member manifests should live under crates/: {packages:#?}"
    );

    Ok(())
}

#[test]
fn repo_uses_skills_directory_as_the_bundle_source_of_truth() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    for relative_path in [
        "skills/dev-registry.toml",
        "skills/submit-loop/SKILL.md",
        "skills/submit-loop/coordinator.md",
        "skills/submit-loop/submit-loop.toml",
        "skills/submit-loop/bundle.toml",
        "skills/gen-plan/SKILL.md",
        "skills/gen-plan/gen-plan.toml",
        "skills/gen-plan/bundle.toml",
        "skills/execute-plan/SKILL.md",
        "skills/execute-plan/execute-plan.toml",
        "skills/execute-plan/bundle.toml",
    ] {
        assert!(
            repo_root.join(relative_path).exists(),
            "expected bundle source asset at {}",
            repo_root.join(relative_path).display()
        );
    }

    Ok(())
}
