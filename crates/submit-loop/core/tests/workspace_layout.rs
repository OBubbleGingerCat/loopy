mod support;

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use loopy_common_bundle::{LoaderRegistration, dispatch_loader, resolve_development_skill};
use loopy_execute_plan_bundle as execute_plan_bundle;
use loopy_gen_plan_bundle as gen_plan_bundle;
use loopy_submit_loop_bundle as submit_loop_bundle;
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

type LoaderValidator = fn(&Path) -> Result<()>;

fn validate_submit_loop_bundle(bundle_root: &Path) -> Result<()> {
    submit_loop_bundle::load_bundle_descriptor(bundle_root).map(|_| ())
}

fn validate_gen_plan_bundle(bundle_root: &Path) -> Result<()> {
    gen_plan_bundle::validate_placeholder_bundle(bundle_root)
}

fn validate_execute_plan_bundle(bundle_root: &Path) -> Result<()> {
    execute_plan_bundle::validate_placeholder_bundle(bundle_root)
}

#[test]
fn dev_registry_and_loader_dispatch_cover_all_workspace_skills() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let loader_registrations = [
        LoaderRegistration {
            loader_id: submit_loop_bundle::LOADER_ID,
            loader: validate_submit_loop_bundle as LoaderValidator,
        },
        LoaderRegistration {
            loader_id: gen_plan_bundle::LOADER_ID,
            loader: validate_gen_plan_bundle as LoaderValidator,
        },
        LoaderRegistration {
            loader_id: execute_plan_bundle::LOADER_ID,
            loader: validate_execute_plan_bundle as LoaderValidator,
        },
    ];

    for (skill_id, expected_root, expected_package, expected_binary_name) in [
        (
            "loopy:submit-loop",
            crate::support::submit_loop_source_root().clone(),
            "loopy-submit-loop",
            "loopy-submit-loop",
        ),
        (
            "loopy:gen-plan",
            repo_root.join("skills/gen-plan"),
            "loopy-gen-plan",
            "loopy-gen-plan",
        ),
        (
            "loopy:execute-plan",
            repo_root.join("skills/execute-plan"),
            "loopy-execute-plan",
            "loopy-execute-plan",
        ),
    ] {
        let resolved = resolve_development_skill(repo_root, skill_id)?;
        assert_eq!(resolved.bundle_root, expected_root);
        assert_eq!(resolved.registration.binary_package, expected_package);
        assert_eq!(resolved.registration.binary_name, expected_binary_name);
        let validate_bundle = dispatch_loader(&resolved.descriptor.loader_id, &loader_registrations)?;
        validate_bundle(&resolved.bundle_root)?;
    }

    Ok(())
}
