mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use support::ensure_prompt_covers_required_help_flags;

#[test]
fn installer_builds_and_copies_the_submit_loop_skill_bundle() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;

    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(install_root.join("SKILL.md").is_file());
    assert!(install_root.join("submit-loop.toml").is_file());
    assert!(install_root.join("coordinator.md").is_file());
    assert!(
        install_root
            .join("roles/coding-task/task-type.toml")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/planning_worker/codex_planner.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_worker/codex_implementer.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/codex_scope.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/codex_plan.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/codex_contract.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/codex_checkpoint_contract.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/codex_correctness.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/codex_code_quality.md")
            .is_file()
    );

    let bundled_binary = install_root.join("bin/loopy-submit-loop");
    assert!(bundled_binary.is_file());

    let mode = fs::metadata(&bundled_binary)?.permissions().mode();
    assert_ne!(mode & 0o111, 0, "expected bundled binary to be executable");

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.starts_with("---\n"),
        "installed SKILL.md should be loadable by Codex as a real skill"
    );
    assert!(
        installed_skill.contains("name: \"loopy:submit-loop\""),
        "installed SKILL.md should declare the loopy:submit-loop skill name"
    );
    assert!(
        installed_skill.contains("./bin/loopy-submit-loop")
            || installed_skill.contains("bin/loopy-submit-loop")
            || installed_skill.contains("${SKILL_ROOT}/bin/loopy-submit-loop"),
        "installed SKILL.md should use the bundled CLI path"
    );
    assert!(
        installed_skill.contains("coordinator.md"),
        "installed SKILL.md should hand off to the top-level coordinator prompt"
    );
    assert!(
        !installed_skill.contains("roles/coordinator.md"),
        "installed SKILL.md should stop referencing the legacy coordinator path"
    );
    assert!(
        installed_skill.contains("spawn_agent")
            || installed_skill.contains("dedicated coordinator subagent"),
        "installed SKILL.md should describe the dedicated coordinator subagent launch"
    );
    assert!(
        !installed_skill.contains("codex exec --full-auto"),
        "installed SKILL.md should stop hard-coding a nested codex exec coordinator launch"
    );
    assert!(
        installed_skill.contains("same model")
            || installed_skill.contains("caller") && installed_skill.contains("model"),
        "installed SKILL.md should document that the coordinator subagent uses the caller model"
    );
    assert!(
        installed_skill.contains("open-loop"),
        "installed SKILL.md should open the loop before delegating to the coordinator"
    );
    assert!(
        installed_skill.contains("finalize-failure"),
        "installed SKILL.md should describe the coordinator-failure terminal path through finalize-failure"
    );
    assert!(
        installed_skill.contains("begin-caller-finalize"),
        "installed SKILL.md should instruct the caller to claim caller-owned finalize after coordinator handoff"
    );
    assert!(
        installed_skill.contains("block-caller-finalize"),
        "installed SKILL.md should document the blocked human-handoff runtime command"
    );
    assert!(
        installed_skill.contains("integration-summary-json"),
        "installed SKILL.md should show how caller-owned finalize-success passes the integration summary"
    );
    assert!(
        !installed_skill.contains("integrate-accepted-commits"),
        "installed SKILL.md should stop telling the caller or coordinator to use the old coordinator-owned integration command"
    );
    assert!(
        !installed_skill.contains("fail-loop") && !installed_skill.contains("build-failure-result"),
        "installed SKILL.md should remove the old split failure commands"
    );
    assert!(
        installed_skill.contains("exits non-zero")
            || installed_skill.contains("returns a non-zero exit status"),
        "installed SKILL.md should describe how non-zero coordinator exits are handled"
    );
    assert!(
        installed_skill.contains("Repeated artifact rounds")
            || installed_skill.contains("Repeated artifact review rounds"),
        "installed SKILL.md should warn that repeated artifact rounds alone are not evidence of coordinator failure"
    );
    assert!(
        installed_skill.contains("does not transfer coordinator ownership"),
        "installed SKILL.md should explain that polling does not transfer coordinator ownership back to the caller"
    );
    assert!(
        installed_skill.contains("request-timeout-extension")
            && installed_skill.contains("does not take effect immediately")
            && installed_skill.contains("proportionate timeout increase"),
        "installed SKILL.md should document advisory timeout-extension semantics and runtime-owned retry policy"
    );
    assert!(
        installed_skill.contains("\"improvement_opportunities\": []"),
        "installed SKILL.md should document caller-facing improvement aggregation in the success result"
    );
    assert!(
        installed_skill.contains("constraints.smoke_mode")
            && installed_skill.contains("\"worker_blocked\""),
        "installed SKILL.md should document reserved smoke-mode constraints for binary-only callers"
    );
    assert!(
        installed_skill.contains("status")
            && installed_skill.contains("phase")
            && installed_skill.contains("plan.latest_submitted_plan_revision")
            && installed_skill.contains("plan.current_executable_plan_revision")
            && installed_skill.contains("latest_invocation")
            && installed_skill.contains("latest_review")
            && installed_skill.contains("result")
            && installed_skill.contains("caller_finalize"),
        "installed SKILL.md should document the caller-visible show-loop --json polling fields"
    );
    assert!(
        contains_normalized_snippet(
            &installed_skill,
            "{\"strategy\":\"cherry_pick\",\"landed_commit_shas\":[\"abc123\"],\"resolution_notes\":null}",
        ),
        "installed SKILL.md should document the accepted finalize-success integration_summary_json request shape"
    );
    assert!(
        installed_skill.contains("conflicting_files_json")
            && installed_skill.contains("[\"src/foo.rs\", \"Cargo.toml\"]"),
        "installed SKILL.md should document conflicting_files_json as an array of strings"
    );

    let installed_coordinator = fs::read_to_string(install_root.join("coordinator.md"))?;
    assert!(
        installed_coordinator.contains(".loopy/git-common-"),
        "installed coordinator should describe the writable gitdir mirror fallback for sandboxed worktree creation"
    );
    assert!(
        installed_coordinator.contains("git --git-dir="),
        "installed coordinator should document the explicit gitdir-based worktree creation fallback"
    );
    assert!(
        installed_coordinator.contains(
            "If `prepare-worktree` returns a failure result, return it immediately instead of continuing to step 2."
        ),
        "installed coordinator should stop immediately after prepare-worktree materializes a terminal failure result"
    );
    assert!(
        installed_coordinator.contains("handoff-to-caller-finalize"),
        "installed coordinator should hand off to the caller instead of mutating the caller branch"
    );
    assert!(
        installed_coordinator.contains(
            "Do not infer that the loop is ready for handoff from a single artifact approval"
        ),
        "installed coordinator should forbid treating a single artifact approval as handoff-ready"
    );
    assert!(
        installed_coordinator
            .contains("Only call `handoff-to-caller-finalize` when every active checkpoint")
            || installed_coordinator
                .contains("every active checkpoint in the current executable plan is accepted"),
        "installed coordinator should require every active checkpoint to be accepted before handoff"
    );
    assert!(
        installed_coordinator.contains("lowest-sequence remaining checkpoint"),
        "installed coordinator should continue with the next remaining checkpoint instead of handing off early"
    );
    assert!(
        !installed_coordinator.contains("\"$SKILL_ROOT/bin/loopy-submit-loop\" finalize-success"),
        "installed coordinator should stop materializing terminal success itself"
    );
    assert!(
        !installed_coordinator.contains("integrate-accepted-commits"),
        "installed coordinator should stop using the old coordinator-owned integration command"
    );
    assert!(
        !installed_coordinator.contains("record-worktree-deleted")
            && !installed_coordinator.contains("build-success-result"),
        "installed coordinator should no longer manually sequence legacy success-finalization commands"
    );
    assert!(
        !installed_coordinator.contains("record-worktree-created")
            && !installed_coordinator.contains("record-worktree-create-failed")
            && !installed_coordinator.contains("open-worker-invocation")
            && !installed_coordinator.contains("open-reviewer-invocation")
            && !installed_coordinator.contains("dispatch-invocation")
            && !installed_coordinator.contains("fail-loop")
            && !installed_coordinator.contains("build-failure-result"),
        "installed coordinator should not mention removed split coordinator commands"
    );
    assert!(
        !installed_coordinator.contains("Open the loop with the caller request object"),
        "installed coordinator should no longer own loop creation once the caller pre-opens the loop"
    );
    assert!(
        installed_coordinator.contains("request-timeout-extension")
            && installed_coordinator.contains("progress evidence")
            && installed_coordinator.contains("proportionate timeout increase")
            && installed_coordinator.contains("five attempts total per invocation"),
        "installed coordinator should document advisory timeout-extension handling and the five-attempt retry cap"
    );
    assert!(
        installed_coordinator.contains("constraints.smoke_mode")
            && installed_coordinator.contains("\"worker_blocked\""),
        "installed coordinator should document the reserved smoke-mode constraint key and value"
    );
    assert!(
        installed_coordinator.contains("show-loop")
            && installed_coordinator.contains("status")
            && installed_coordinator.contains("phase")
            && installed_coordinator.contains("plan.latest_submitted_plan_revision")
            && installed_coordinator.contains("plan.current_executable_plan_revision")
            && installed_coordinator.contains("latest_invocation.accepted_api")
            && installed_coordinator.contains("latest_review.round_status")
            && installed_coordinator.contains("result.status"),
        "installed coordinator should document the authoritative show-loop JSON polling fields it may use"
    );
    assert!(
        !installed_coordinator.contains("../SKILL.md"),
        "installed coordinator should not depend on external prompt files for mandatory contract details"
    );

    Ok(())
}

#[test]
fn installer_emits_loader_descriptor_and_renamed_submit_loop_binary() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;

    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let descriptor_path = install_root.join("bundle.toml");
    assert!(
        descriptor_path.is_file(),
        "expected installed descriptor at {}",
        descriptor_path.display()
    );

    let descriptor = fs::read_to_string(&descriptor_path)?;
    assert!(
        descriptor.contains("skill_id = \"loopy:submit-loop\""),
        "descriptor should declare the installed skill id"
    );
    assert!(
        descriptor.contains("skill_kind"),
        "descriptor should declare the skill kind"
    );
    assert!(
        descriptor.contains("loader_id"),
        "descriptor should declare the loader id"
    );
    assert!(
        descriptor.contains("bin/loopy-submit-loop"),
        "descriptor should point at the renamed installed binary"
    );

    let renamed_binary = install_root.join("bin/loopy-submit-loop");
    assert!(
        renamed_binary.is_file(),
        "expected installed binary at {}",
        renamed_binary.display()
    );
    assert!(
        !install_root.join("bin/loopy").exists(),
        "legacy bin/loopy path should no longer be the installed runtime entrypoint"
    );

    Ok(())
}

#[test]
fn installed_bundle_skill_mentions_show_loop_status_query() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.contains("\"$SKILL_ROOT/bin/loopy-submit-loop\" show-loop --loop-id <loop_id>"),
        "installed SKILL.md should document the show-loop status query with the bundled binary path and an explicit loop id"
    );
    assert!(
        installed_skill.contains("--workspace <workspace_root>"),
        "installed SKILL.md should document how callers pass the original workspace root when polling from another cwd"
    );
    assert!(
        installed_skill.contains("--json"),
        "installed SKILL.md should advertise --json for machine-readable polling"
    );
    assert!(
        installed_skill.contains("read-only status inspection"),
        "installed SKILL.md should explain that status polling is observational only"
    );
    assert!(
        installed_skill.contains("periodically use this read-only query as a health check"),
        "installed SKILL.md should recommend periodic health checks while waiting for the coordinator"
    );
    assert!(
        installed_skill.contains("does not transfer coordinator ownership"),
        "installed SKILL.md should explain that status polling does not transfer coordinator ownership"
    );
    assert!(
        installed_skill.contains("status")
            && installed_skill.contains("phase")
            && installed_skill.contains("plan.latest_submitted_plan_revision")
            && installed_skill.contains("plan.current_executable_plan_revision")
            && installed_skill.contains("latest_invocation")
            && installed_skill.contains("latest_review")
            && installed_skill.contains("result")
            && installed_skill.contains("caller_finalize"),
        "installed SKILL.md should document the caller-visible show-loop --json fields used for polling"
    );

    Ok(())
}

#[test]
fn installed_bundle_documents_bypass_sandbox_and_bypass_manifest_variants() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.contains("\"bypass_sandbox\": false"),
        "installed SKILL.md should include bypass_sandbox in the request example"
    );
    assert!(
        installed_skill.contains("`bypass_sandbox` is optional and defaults to `false`."),
        "installed SKILL.md should document that bypass_sandbox is optional and defaults to false"
    );
    assert!(
        installed_skill.contains(
            "nested worker/reviewer execution uses the bypass executor variant and inherits the caller environment."
        ),
        "installed SKILL.md should explain that bypass_sandbox switches nested worker/reviewer execution to the bypass executor variant and inherits the caller environment"
    );

    let installed_manifest = fs::read_to_string(install_root.join("submit-loop.toml"))?;
    assert!(
        installed_manifest.contains("bypass_sandbox_args = ["),
        "installed submit-loop.toml should define bypass_sandbox_args for executor variants"
    );
    assert!(
        installed_manifest.contains("bypass_sandbox_inherit_env = true"),
        "installed submit-loop.toml should preserve bypass_sandbox_inherit_env in the installed bundle"
    );

    Ok(())
}

#[test]
fn installed_worker_roles_are_stage_specific() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let planner_role = fs::read_to_string(
        install_root.join("roles/coding-task/planning_worker/codex_planner.md"),
    )?;
    assert!(
        planner_role.contains("repository-grounded")
            && planner_role.contains("deliverables")
            && planner_role.contains("verification"),
        "installed planning worker role should stay focused on repository-grounded planning and checkpoint contract quality"
    );
    assert!(
        !planner_role.contains("declare-worker-blocked") && !planner_role.contains("bundle_bin"),
        "installed planning worker role should not duplicate runtime-owned protocol CLI text"
    );

    let implementer_role = fs::read_to_string(
        install_root.join("roles/coding-task/artifact_worker/codex_implementer.md"),
    )?;
    assert!(
        implementer_role.contains("smallest defensible change set")
            && implementer_role.contains("verification evidence")
            && implementer_role.contains("follow-up"),
        "installed artifact worker role should stay focused on minimal implementation and caller-facing follow-up ideas"
    );
    assert!(
        !implementer_role.contains("declare-worker-blocked")
            && !implementer_role.contains("bundle_bin"),
        "installed artifact worker role should not duplicate runtime-owned protocol CLI text"
    );

    Ok(())
}

#[test]
fn installer_copies_task_type_role_tree_and_top_level_coordinator() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(install_root.join("coordinator.md").is_file());
    assert!(
        install_root
            .join("roles/coding-task/task-type.toml")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/planning_worker/codex_planner.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/planning_worker/mock_planner.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_worker/codex_implementer.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_worker/mock_implementer.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/codex_scope.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/codex_plan.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/codex_contract.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/checkpoint_reviewer/mock.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/codex_checkpoint_contract.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/codex_correctness.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/codex_code_quality.md")
            .is_file()
    );
    assert!(
        install_root
            .join("roles/coding-task/artifact_reviewer/mock.md")
            .is_file()
    );
    assert!(
        !install_root.join("roles/coding-task/worker").exists(),
        "installer should remove the legacy shared worker role directory"
    );

    Ok(())
}

#[test]
fn installer_no_longer_copies_legacy_flat_role_files() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(!install_root.join("roles/coordinator.md").exists());
    assert!(!install_root.join("roles/worker.md").exists());
    assert!(!install_root.join("roles/checkpoint_reviewer.md").exists());
    assert!(!install_root.join("roles/artifact_reviewer.md").exists());
    assert!(
        !install_root
            .join("roles/coding-task/checkpoint_reviewer/default.md")
            .exists()
    );
    assert!(
        !install_root
            .join("roles/coding-task/artifact_reviewer/default.md")
            .exists()
    );

    Ok(())
}

#[test]
fn installer_uses_online_resolution_by_default_and_keeps_env_driven_offline_mode() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let temp = tempfile::tempdir()?;
    let fake_codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&fake_codex_home)?;

    let logs = run_installer_with_fake_cargo_and_args(
        repo_root,
        &[],
        &[("CODEX_HOME", fake_codex_home.as_path())],
        None,
    )?;
    assert!(
        !logs.args.contains("--offline"),
        "installer should not force --offline by default: {}",
        logs.args
    );
    assert!(
        logs.offline_env.is_empty(),
        "unexpected default CARGO_NET_OFFLINE value: {}",
        logs.offline_env
    );

    let logs = run_installer_with_fake_cargo_and_args(
        repo_root,
        &[],
        &[("CODEX_HOME", fake_codex_home.as_path())],
        Some("true"),
    )?;
    assert!(
        logs.args.contains("--offline"),
        "installer should pass --offline when CARGO_NET_OFFLINE is set: {}",
        logs.args
    );
    assert_eq!(logs.offline_env, "true");

    Ok(())
}

#[test]
fn installer_supports_codex_and_claude_host_targets() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let temp = tempfile::tempdir()?;
    let fake_home = temp.path().join("home");
    let fake_codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&fake_home)?;
    fs::create_dir_all(&fake_codex_home)?;

    let codex = run_installer_with_fake_cargo_and_args(
        repo_root,
        &["--target", "codex"],
        &[
            ("HOME", fake_home.as_path()),
            ("CODEX_HOME", fake_codex_home.as_path()),
        ],
        None,
    )?;
    assert_eq!(
        codex.install_root,
        fake_codex_home.join("skills").join("loopy-submit-loop")
    );
    assert!(codex.install_root.join("SKILL.md").is_file());

    let claude = run_installer_with_fake_cargo_and_args(
        repo_root,
        &["--target", "claude"],
        &[("HOME", fake_home.as_path())],
        None,
    )?;
    assert_eq!(
        claude.install_root,
        fake_home
            .join(".claude")
            .join("skills")
            .join("loopy-submit-loop")
    );
    assert!(claude.install_root.join("SKILL.md").is_file());

    Ok(())
}

#[test]
fn host_installed_bundle_still_uses_workspace_local_loopy_db() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let temp = tempfile::tempdir()?;
    let fake_codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&fake_codex_home)?;
    let install_root = fake_codex_home.join("skills").join("loopy-submit-loop");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .args(["--target", "codex"])
        .env("CODEX_HOME", &fake_codex_home)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to install bundle into fake CODEX_HOME")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let workspace = tempfile::tempdir()?;
    let init = Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(workspace.path())
        .output()
        .context("failed to init git workspace")?;
    if !init.status.success() {
        bail!(
            "git init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
    }
    let config_name = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(workspace.path())
        .output()
        .context("failed to configure git user.name in workspace")?;
    if !config_name.status.success() {
        bail!(
            "git config user.name failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&config_name.stdout),
            String::from_utf8_lossy(&config_name.stderr)
        );
    }
    let config_email = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(workspace.path())
        .output()
        .context("failed to configure git user.email in workspace")?;
    if !config_email.status.success() {
        bail!(
            "git config user.email failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&config_email.stdout),
            String::from_utf8_lossy(&config_email.stderr)
        );
    }
    fs::write(workspace.path().join("README.md"), "fixture\n")?;
    let add = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(workspace.path())
        .output()
        .context("failed to stage initial fixture file")?;
    if !add.status.success() {
        bail!(
            "git add failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&add.stdout),
            String::from_utf8_lossy(&add.stderr)
        );
    }
    let commit = Command::new("git")
        .args(["commit", "-m", "initial fixture"])
        .current_dir(workspace.path())
        .output()
        .context("failed to create initial commit in workspace")?;
    if !commit.status.success() {
        bail!(
            "git commit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&commit.stdout),
            String::from_utf8_lossy(&commit.stderr)
        );
    }

    let open_loop = Command::new(install_root.join("bin/loopy-submit-loop"))
        .args([
            "open-loop",
            "--summary",
            "host install",
            "--task-type",
            "coding-task",
            "--context",
            "runtime stays local",
        ])
        .current_dir(workspace.path())
        .output()
        .context("failed to run host-installed bin/loopy-submit-loop open-loop")?;
    if !open_loop.status.success() {
        bail!(
            "open-loop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&open_loop.stdout),
            String::from_utf8_lossy(&open_loop.stderr)
        );
    }

    let payload: Value = serde_json::from_slice(&open_loop.stdout)?;
    let db_path = PathBuf::from(
        payload["db_path"]
            .as_str()
            .context("missing db_path in open-loop response")?,
    );
    assert_eq!(db_path, workspace.path().join(".loopy/loopy.db"));
    assert!(db_path.is_file());
    assert!(
        !install_root.join(".loopy/loopy.db").exists(),
        "host skill directory must not become the runtime root"
    );

    Ok(())
}

#[test]
fn installed_bundle_metadata_describes_host_skill_installation() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.contains("$CODEX_HOME/skills/loopy-submit-loop"),
        "installed SKILL.md should describe the Codex host skill root"
    );
    assert!(
        installed_skill.contains("~/.claude/skills/loopy-submit-loop"),
        "installed SKILL.md should describe the Claude Code host skill root"
    );

    let manifest = fs::read_to_string(install_root.join("submit-loop.toml"))?;
    assert!(
        !manifest.contains(".loopy/installed-skills/loopy-submit-loop"),
        "installed manifest should stop advertising the workspace runtime directory as the default install root"
    );
    assert!(
        manifest.contains("default_install_target = \"codex\""),
        "installed manifest should describe the default host install target"
    );
    assert!(
        manifest.contains("--add-dir"),
        "installed manifest should allow nested codex runs to write the shared .loopy directory"
    );
    assert!(
        manifest.contains("{workspace_root}/.loopy"),
        "installed manifest should template the shared .loopy directory from the workspace root"
    );

    Ok(())
}

#[test]
fn installed_bundle_contract_requires_minimal_context_coordinator_launch() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.contains("Do not fork the caller thread history"),
        "installed SKILL.md should require launching the coordinator without inheriting the caller transcript"
    );
    assert!(
        installed_skill.contains("Pass only the exact installed `coordinator.md` prompt"),
        "installed SKILL.md should constrain the coordinator launch payload to the installed prompt plus authoritative loop metadata"
    );

    let installed_coordinator = fs::read_to_string(install_root.join("coordinator.md"))?;
    assert!(
        installed_coordinator.contains("Start with execution outline step 1 immediately"),
        "installed coordinator should require execution to begin at worktree creation instead of exploratory preflight"
    );
    assert!(
        installed_coordinator.contains("Do not begin by inspecting existing repo state"),
        "installed coordinator should forbid exploratory repo or loop inspection before a runtime step fails"
    );
    assert!(
        installed_coordinator.contains("finalize-failure"),
        "installed coordinator should route coordinator-owned terminal failure through finalize-failure"
    );
    assert!(
        installed_coordinator.contains("plan_rejected")
            && installed_coordinator.contains("planning"),
        "installed coordinator should describe the valid plan_rejected -> planning transition"
    );
    assert!(
        installed_coordinator.contains("reopen")
            && installed_coordinator.contains("planning worker"),
        "installed coordinator should reopen planning after a rejected plan instead of failing immediately"
    );

    let installed_checkpoint_scope = fs::read_to_string(
        install_root.join("roles/coding-task/checkpoint_reviewer/codex_scope.md"),
    )?;
    assert!(
        installed_checkpoint_scope.contains("scope")
            && installed_checkpoint_scope.contains("reviewable unit"),
        "installed checkpoint scope reviewer should stay focused on checkpoint boundary quality"
    );
    let installed_checkpoint_contract = fs::read_to_string(
        install_root.join("roles/coding-task/checkpoint_reviewer/codex_contract.md"),
    )?;
    assert!(
        installed_checkpoint_contract.contains("verification")
            && installed_checkpoint_contract.contains("deliverables")
            && installed_checkpoint_contract.contains("acceptance"),
        "installed checkpoint contract reviewer should stay focused on verification and contract closure"
    );

    Ok(())
}

#[test]
fn installed_bundle_documents_exact_cli_forms_for_prompts() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" open-loop --summary <summary> --task-type <task_type>"
        ),
        "installed SKILL.md should document the exact open-loop base CLI form"
    );
    assert!(
        installed_skill.contains("--context <context>")
            && installed_skill.contains("--planning-worker <planning_worker>")
            && installed_skill.contains("--artifact-worker <artifact_worker>")
            && installed_skill.contains("--checkpoint-reviewers-json <json_array>")
            && installed_skill.contains("--artifact-reviewers-json <json_array>")
            && installed_skill.contains("--constraints-json <json_object>")
            && installed_skill.contains("--bypass-sandbox"),
        "installed SKILL.md should document the exact optional open-loop flags"
    );
    assert!(
        installed_skill.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" finalize-failure --loop-id <loop_id> --failure-cause-type coordinator_failure --summary <summary>"
        ),
        "installed SKILL.md should document the exact caller-side finalize-failure CLI form"
    );

    let installed_coordinator = fs::read_to_string(install_root.join("coordinator.md"))?;
    assert!(
        installed_coordinator
            .contains("\"$SKILL_ROOT/bin/loopy-submit-loop\" prepare-worktree --loop-id <loop_id>"),
        "installed coordinator should document the exact prepare-worktree CLI form"
    );
    assert!(
        installed_coordinator.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" start-worker-invocation --loop-id <loop_id> --stage planning"
        ),
        "installed coordinator should document the exact planning worker CLI form"
    );
    assert!(
        installed_coordinator.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" open-review-round --loop-id <loop_id> --review-kind checkpoint --target-type plan_revision --target-ref plan-<revision>"
        ),
        "installed coordinator should document the exact checkpoint review round CLI form"
    );
    assert!(
        installed_coordinator.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" start-worker-invocation --loop-id <loop_id> --stage artifact --checkpoint-id <checkpoint_id>"
        ),
        "installed coordinator should document the exact artifact worker CLI form"
    );
    assert!(
        installed_coordinator.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" open-review-round --loop-id <loop_id> --review-kind artifact --target-type checkpoint_id --target-ref <checkpoint_id>"
        ),
        "installed coordinator should document the exact artifact review round CLI form"
    );
    assert!(
        installed_coordinator.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" start-reviewer-invocation --loop-id <loop_id> --review-round-id <review_round_id> --review-slot-id <review_slot_id>"
        ),
        "installed coordinator should document the exact reviewer invocation CLI form"
    );
    assert!(
        installed_coordinator.contains("blocking_issues")
            && installed_coordinator.contains("nonblocking_issues"),
        "installed coordinator should describe reviewer revision context in the redesigned structured issue terms"
    );
    assert!(
        !installed_coordinator.contains("issues and notes"),
        "installed coordinator should stop describing revision context as legacy issues-and-notes text"
    );
    assert!(
        installed_coordinator
            .contains("\"$SKILL_ROOT/bin/loopy-submit-loop\" handoff-to-caller-finalize --loop-id <loop_id>"),
        "installed coordinator should document the exact handoff-to-caller-finalize CLI form"
    );
    assert!(
        installed_skill
            .contains("\"$SKILL_ROOT/bin/loopy-submit-loop\" begin-caller-finalize --loop-id <loop_id>"),
        "installed SKILL.md should document the exact begin-caller-finalize CLI form"
    );
    assert!(
        installed_skill.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" block-caller-finalize --loop-id <loop_id> --strategy-summary <summary> --blocking-summary <summary> --human-question <question> --conflicting-files-json <json_array>"
        ),
        "installed SKILL.md should document the exact block-caller-finalize CLI form"
    );
    assert!(
        installed_skill.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" finalize-success --loop-id <loop_id> --integration-summary-json <json_object>"
        ),
        "installed SKILL.md should document the exact caller-owned finalize-success CLI form"
    );
    assert!(
        installed_coordinator.contains(
            "\"$SKILL_ROOT/bin/loopy-submit-loop\" finalize-failure --loop-id <loop_id> --failure-cause-type coordinator_failure --summary <summary>"
        ),
        "installed coordinator should document the exact finalize-failure CLI form"
    );

    Ok(())
}

#[test]
fn installed_bundle_prompt_docs_track_cli_help_required_flags() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let install_base = tempfile::tempdir()?;
    let install_root = install_base.path().join("submit-loop-skill");

    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let bundled_loopy = install_root.join("bin/loopy-submit-loop");
    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    let installed_coordinator = fs::read_to_string(install_root.join("coordinator.md"))?;

    ensure_prompt_covers_required_help_flags(&bundled_loopy, &installed_skill, "open-loop")?;
    ensure_prompt_covers_required_help_flags(&bundled_loopy, &installed_skill, "show-loop")?;
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &installed_skill,
        "begin-caller-finalize",
    )?;
    ensure_prompt_covers_required_help_flags(
        &bundled_loopy,
        &installed_skill,
        "block-caller-finalize",
    )?;
    ensure_prompt_covers_required_help_flags(&bundled_loopy, &installed_skill, "finalize-success")?;
    ensure_prompt_covers_required_help_flags(&bundled_loopy, &installed_skill, "finalize-failure")?;

    for command in [
        "prepare-worktree",
        "start-worker-invocation",
        "open-review-round",
        "start-reviewer-invocation",
        "handoff-to-caller-finalize",
        "finalize-failure",
    ] {
        ensure_prompt_covers_required_help_flags(&bundled_loopy, &installed_coordinator, command)?;
    }

    Ok(())
}

struct FakeCargoRun {
    install_root: PathBuf,
    args: String,
    offline_env: String,
}

fn contains_normalized_snippet(haystack: &str, snippet: &str) -> bool {
    let normalize =
        |value: &str| -> String { value.chars().filter(|ch| !ch.is_whitespace()).collect() };
    normalize(haystack).contains(&normalize(snippet))
}

fn run_installer_with_fake_cargo_and_args(
    repo_root: &Path,
    installer_args: &[&str],
    extra_envs: &[(&str, &Path)],
    offline_env: Option<&str>,
) -> Result<FakeCargoRun> {
    let temp = tempfile::tempdir()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;

    let args_log = temp.path().join("cargo-args.log");
    let env_log = temp.path().join("cargo-env.log");
    let fake_cargo = fake_bin_dir.join("cargo");
    fs::write(
        &fake_cargo,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "{args_log}"
printf '%s\n' "${{CARGO_NET_OFFLINE:-}}" > "{env_log}"
mkdir -p "$PWD/target/$CARGO_BUILD_PROFILE"
cat > "$PWD/target/$CARGO_BUILD_PROFILE/loopy-submit-loop" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$PWD/target/$CARGO_BUILD_PROFILE/loopy-submit-loop"
"#,
            args_log = args_log.display(),
            env_log = env_log.display(),
        ),
    )?;
    let mut perms = fs::metadata(&fake_cargo)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fake_cargo, perms)?;

    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new("bash");
    command
        .arg("scripts/install-submit-loop-skill.sh")
        .args(installer_args)
        .env("PATH", path)
        .env("CARGO_BUILD_PROFILE", "fake-profile")
        .current_dir(repo_root);
    for (key, value) in extra_envs {
        command.env(key, value);
    }
    if let Some(offline_env) = offline_env {
        command.env("CARGO_NET_OFFLINE", offline_env);
    }
    let output = command
        .output()
        .context("failed to run install-submit-loop-skill.sh with fake cargo")?;
    if !output.status.success() {
        bail!(
            "installer with fake cargo failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(FakeCargoRun {
        install_root: PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
        args: fs::read_to_string(args_log)?.trim().to_owned(),
        offline_env: fs::read_to_string(env_log)?.trim().to_owned(),
    })
}
