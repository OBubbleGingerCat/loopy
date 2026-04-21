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
        installed_skill.contains("bundle_bin") && installed_skill.contains("bin/loopy-submit-loop"),
        "installed SKILL.md should derive and pass an explicit bundle_bin path"
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
        installed_skill.contains("workspace_root"),
        "installed SKILL.md should pass the authoritative workspace_root through the caller/coordinator handoff"
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
        installed_skill.contains("worktree_ref.path/.git")
            && installed_skill.contains("format-patch -1 --stdout")
            && installed_skill.contains("strategy = \"replay\"")
            && installed_skill.contains(
                "Do not replace this mirrored caller-git replay path with Python `shutil`"
            ),
        "installed SKILL.md should document replaying accepted commits from the authoritative worktree gitdir when caller cherry-pick cannot resolve the object"
    );
    assert!(
        installed_skill.contains("caller_git_dir")
            && installed_skill.contains("rev-parse --absolute-git-dir")
            && installed_skill.contains(".loopy/caller-git")
            && installed_skill.contains("GIT_DIR=\"$caller_git_dir\"")
            && installed_skill.contains("GIT_WORK_TREE=\"<workspace_root>\""),
        "installed SKILL.md should document the mirrored caller gitdir fallback for read-only caller .git sandboxes"
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
        installed_coordinator.contains("mkdir -p")
            && installed_coordinator.contains("cp -a")
            && installed_coordinator.contains(".git/.")
            && installed_coordinator.contains("show-ref --verify --quiet")
            && installed_coordinator.contains("refs/heads/<branch>"),
        "installed coordinator should spell out the mirrored-gitdir fallback setup and branch-existence probe"
    );
    assert!(
        installed_coordinator.contains("worktree add -b \"<branch>\"")
            && installed_coordinator.contains("worktree add \"<worktree_path>\" \"<branch>\""),
        "installed coordinator should document both mirrored-gitdir worktree-add variants"
    );
    assert!(
        installed_coordinator.contains("rerun `<bundle_bin> prepare-worktree --loop-id <loop_id> --workspace <workspace_root>`")
            || installed_coordinator.contains("re-run `<bundle_bin> prepare-worktree --loop-id <loop_id> --workspace <workspace_root>`"),
        "installed coordinator should require re-entering prepare-worktree after the mirrored-gitdir fallback"
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
        installed_coordinator.contains("SUBMIT_LOOP__submit_checkpoint_plan")
            && installed_coordinator.contains("SUBMIT_LOOP__declare_worker_blocked")
            && installed_coordinator.contains("SUBMIT_LOOP__submit_candidate_commit")
            && installed_coordinator.contains("Do not shorten them"),
        "installed coordinator should compare accepted_terminal_api against the exact runtime API names instead of shorthand labels"
    );
    assert!(
        installed_coordinator.contains(".loopy/loopy.db")
            && installed_coordinator.contains("SUBMIT_LOOP__checkpoint_current")
            && installed_coordinator.contains("execution_state != 'accepted'")
            && installed_coordinator.contains("do not infer checkpoint ids from checkpoint titles"),
        "installed coordinator should document the read-only checkpoint-id lookup against the workspace-local projection database"
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
        installed_skill
            .contains("<bundle_bin> show-loop --loop-id <loop_id> --workspace <workspace_root>"),
        "installed SKILL.md should document the show-loop status query with explicit bundle_bin and workspace_root inputs"
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
        planner_role.contains("Prefer checkpoint contracts that certify the submitted current repository/worktree state")
            && planner_role.contains("Do not introduce committed-candidate refs by default"),
        "installed planning worker role should default to direct current-state verification unless committed candidate refs are truly required"
    );
    assert!(
        !planner_role.contains("declare-worker-blocked") && !planner_role.contains("bundle_bin"),
        "installed planning worker role should not duplicate runtime-owned protocol CLI text"
    );

    let implementer_role = fs::read_to_string(
        install_root.join("roles/coding-task/artifact_worker/codex_implementer.md"),
    )?;
    let planner_role = fs::read_to_string(
        install_root.join("roles/coding-task/planning_worker/codex_planner.md"),
    )?;
    assert!(
        planner_role.contains("tracked-file scope")
            && planner_role.contains("pre-change tracked-file set")
            && planner_role.contains("worktree-versus-index diff checks alone"),
        "installed planning worker role should require stable-baseline verification for tracked-file scope claims"
    );
    assert!(
        planner_role.contains("do not use `--diff-filter=AM`")
            && planner_role.contains("hide deletions, renames, type changes")
            && planner_role.contains("exactly match the allowed deliverable set"),
        "installed planning worker role should forbid filtered diff listings that hide tracked changes when proving exclusive file scope"
    );
    assert!(
        planner_role.contains("Do not use `git diff --name-only HEAD`")
            && planner_role.contains("worktree-versus-current-`HEAD` checks")
            && planner_role.contains("literal tracked-file baseline plus exact current contents"),
        "installed planning worker role should forbid HEAD-relative worktree diff checks for committed candidate scope"
    );
    assert!(
        planner_role.contains("certifying a committed candidate artifact")
            && planner_role.contains("verify deliverable contents from that same committed candidate ref")
            && planner_role.contains("Do not mix worktree file reads with `HEAD^..HEAD`"),
        "installed planning worker role should require committed-artifact content checks to use the same committed ref as scope verification"
    );
    assert!(
        planner_role.contains("If any verification step uses committed candidate refs such as `HEAD^..HEAD`")
            && planner_role.contains("treat the entire checkpoint contract as certifying a committed candidate artifact")
            && planner_role.contains("do not mix in current-worktree file reads"),
        "installed planning worker role should force all verification onto the committed-ref basis once any committed candidate refs are introduced"
    );
    assert!(
        planner_role.contains("bind tree and diff queries to an explicit candidate commit ref or SHA")
            && planner_role.contains("floating current-branch names such as bare `HEAD` and `HEAD^` alone")
            && planner_role.contains("derive it from that explicit candidate ref"),
        "installed planning worker role should require committed-candidate verification to bind queries to an explicit candidate ref or SHA"
    );
    assert!(
        planner_role.contains("execute a generated script or other executable deliverable")
            && planner_role.contains("execute bytes materialized from that same explicit candidate ref")
            && planner_role.contains("Do not run `./path` from the current worktree"),
        "installed planning worker role should require executable checks to use the same committed candidate artifact state as the rest of verification"
    );
    assert!(
        planner_role.contains("Before any candidate commit exists")
            && planner_role.contains("use the current checked-out baseline such as `HEAD:<path>`")
            && planner_role.contains("Do not switch that baseline to `HEAD^:<path>`"),
        "installed planning worker role should keep worktree-state verification on the current HEAD baseline unless parent history is explicitly required"
    );
    assert!(
        planner_role.contains("line.endswith(('.', '!', '?'))")
            && planner_role.contains("reviewer-rejected verification snippet unchanged"),
        "installed planning worker role should explicitly forbid malformed Python verification syntax and repeated rejected verification commands"
    );
    assert!(
        planner_role.contains("Prefer a single-quoted shell wrapper with double-quoted Python string literals")
            && planner_role.contains("do not emit nested unescaped quotes"),
        "installed planning worker role should require shell-safe quoting for python -c verification steps"
    );
    assert!(
        planner_role.contains("contains quote characters that would make a `python -c` shell wrapper fragile")
            && planner_role.contains("use a non-piped `python3 - <<'PY'` here-doc")
            && planner_role.contains("shell-malformed"),
        "installed planning worker role should require a safer quoting strategy when python -c would become shell malformed"
    );
    assert!(
        planner_role.contains("do not pipe into `python - <<'PY'`")
            && planner_role.contains("replace stdin with the inline script body")
            && planner_role.contains("use a form such as `python -c` that actually reads the pipeline stdin"),
        "installed planning worker role should forbid here-doc Python pipeline verifiers that cannot read producer stdout"
    );
    assert!(
        planner_role.contains("quote every path, ref, and argv token as a Python string literal")
            && planner_role.contains("README.md")
            && planner_role.contains("docs/proof.txt")
            && planner_role.contains("git")
            && planner_role.contains("HEAD"),
        "installed planning worker role should forbid bare Python identifiers inside verification commands"
    );
    assert!(
        planner_role.contains("only reference `HEAD^`, merge bases, or other parent commits")
            && planner_role.contains("Do not assume the repository has more than one commit")
            && planner_role.contains("nonexistent-history verification command"),
        "installed planning worker role should forbid verification steps that assume nonexistent repository history"
    );
    assert!(
        planner_role.contains("do not build the expected post-change bytes from `HEAD:<path>`")
            && planner_role.contains("double-counting the required edit")
            && planner_role.contains("same checkpoint contract must survive later artifact review")
            && planner_role.contains("Anchor the pre-change bytes to `HEAD^:<path>`"),
        "installed planning worker role should forbid append-only contracts that read the pre-change baseline from candidate HEAD"
    );
    assert!(
        planner_role.contains("subjective size words such as `short`, `brief`, or `small`")
            && planner_role.contains("concrete measurable bound")
            && planner_role.contains("Do not leave text-length expectations implicit"),
        "installed planning worker role should require concrete measurable bounds for subjective text-length requirements"
    );
    assert!(
        planner_role.contains("do not compare current worktree files to `HEAD:<path>`")
            && planner_role.contains("candidate is checked out as `HEAD`")
            && planner_role.contains("certify the candidate tree against itself"),
        "installed planning worker role should forbid HEAD-based tracked-file comparisons that would self-certify the candidate tree"
    );
    assert!(
        planner_role.contains("iterating `git ls-files` and comparing each worktree path to `git show HEAD:{path}`")
            && planner_role.contains("collapses into self-comparison")
            && planner_role.contains("exact current tracked/untracked file sets"),
        "installed planning worker role should forbid per-file HEAD self-comparisons when proving tracked-file scope"
    );
    assert!(
        planner_role.contains("do not use `git ls-tree -r --name-only HEAD`")
            && planner_role.contains("only prove the pre-change commit tree")
            && planner_role.contains("working tree or index state"),
        "installed planning worker role should forbid using the current HEAD tree to prove uncommitted tracked-file contents"
    );
    assert!(
        planner_role.contains("certifying the current worktree submission state")
            && planner_role.contains("commands such as `git ls-files`")
            && planner_role.contains("Do not mix worktree content checks with pre-change commit-tree scope checks"),
        "installed planning worker role should require current-state tracked-file queries for worktree submission scope"
    );
    assert!(
        implementer_role.contains("smallest defensible change set")
            && implementer_role.contains("verification evidence")
            && implementer_role.contains("follow-up"),
        "installed artifact worker role should stay focused on minimal implementation and caller-facing follow-up ideas"
    );
    assert!(
        implementer_role.contains("Once acceptance passes")
            && implementer_role.contains("git rev-parse HEAD")
            && implementer_role.contains("submit that candidate"),
        "installed artifact worker role should describe the direct acceptance-to-commit submission sequence"
    );
    assert!(
        implementer_role.contains("do not repoint `.git`")
            || implementer_role.contains("alternate git metadata"),
        "installed artifact worker role should forbid candidate commits that depend on private git metadata detours"
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
    assert!(
        installed_checkpoint_scope.contains("`git ls-tree -r --name-only HEAD`")
            && installed_checkpoint_scope.contains("current commit tree")
            && installed_checkpoint_scope.contains("does not prove an uncommitted submission state"),
        "installed checkpoint scope reviewer should reject HEAD-tree checks that claim to prove post-change repository contents"
    );
    assert!(
        installed_checkpoint_scope.contains("`--diff-filter=AM`")
            && installed_checkpoint_scope.contains("hide deletions, renames, type changes")
            && installed_checkpoint_scope.contains("full changed tracked-file set"),
        "installed checkpoint scope reviewer should reject filtered diff listings that cannot prove no other tracked changes"
    );
    assert!(
        installed_checkpoint_scope.contains("mixes worktree content reads with committed-ref scope checks")
            && installed_checkpoint_scope.contains("`HEAD^..HEAD`")
            && installed_checkpoint_scope.contains("same committed artifact state"),
        "installed checkpoint scope reviewer should reject mixed worktree-versus-commit proofs for committed artifacts"
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
    assert!(
        installed_checkpoint_contract.contains("expected post-change bytes from `HEAD:<path>`")
            && installed_checkpoint_contract.contains("candidate commit checked out as `HEAD`")
            && installed_checkpoint_contract.contains("explicit pre-change basis such as `HEAD^:<path>`"),
        "installed checkpoint contract reviewer should reject append-only contracts that double-count edits from candidate HEAD"
    );
    assert!(
        installed_checkpoint_contract.contains("pipe data into `python - <<'PY'`")
            && installed_checkpoint_contract.contains("the here-doc consumes stdin")
            && installed_checkpoint_contract.contains("does not execute as claimed"),
        "installed checkpoint contract reviewer should reject Python here-doc pipeline verifiers that cannot consume the intended stdin"
    );
    assert!(
        installed_checkpoint_contract.contains("shell quoting or Python quoting is malformed")
            && installed_checkpoint_contract.contains("bare identifiers where string literals are required"),
        "installed checkpoint contract reviewer should reject malformed quoting and bare Python identifiers in verification commands"
    );
    assert!(
        installed_checkpoint_contract.contains("`git diff --name-only HEAD`")
            && installed_checkpoint_contract.contains("worktree-versus-current-`HEAD` comparisons")
            && installed_checkpoint_contract.contains("still proves scope with the candidate commit checked out as `HEAD`"),
        "installed checkpoint contract reviewer should reject HEAD-relative scope checks for committed candidates"
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
            "<bundle_bin> open-loop --summary <summary> --task-type <task_type> --workspace <workspace_root>"
        ),
        "installed SKILL.md should document the exact open-loop base CLI form with explicit bundle_bin and workspace_root inputs"
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
        installed_skill.contains("shell-sensitive characters")
            && installed_skill.contains("Do not embed those values raw inside a double-quoted `bash -lc` string")
            && installed_skill.contains("subprocess.run([...], check=True)"),
        "installed SKILL.md should require argv-safe caller-side runtime invocation when summary, context, or JSON values contain shell metacharacters"
    );
    assert!(
        installed_skill.contains(
            "<bundle_bin> finalize-failure --loop-id <loop_id> --failure-cause-type coordinator_failure --summary <summary> --workspace <workspace_root>"
        ),
        "installed SKILL.md should document the exact caller-side finalize-failure CLI form with explicit bundle_bin and workspace_root inputs"
    );

    let installed_coordinator = fs::read_to_string(install_root.join("coordinator.md"))?;
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> prepare-worktree --loop-id <loop_id> --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact prepare-worktree CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> start-worker-invocation --loop-id <loop_id> --stage planning --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact planning worker CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> open-review-round --loop-id <loop_id> --review-kind checkpoint --target-type plan_revision --target-ref plan-<revision> --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact checkpoint review round CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> start-worker-invocation --loop-id <loop_id> --stage artifact --checkpoint-id <checkpoint_id> --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact artifact worker CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> open-review-round --loop-id <loop_id> --review-kind artifact --target-type checkpoint_id --target-ref <checkpoint_id> --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact artifact review round CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> start-reviewer-invocation --loop-id <loop_id> --review-round-id <review_round_id> --review-slot-id <review_slot_id> --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact reviewer invocation CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains("latest_invocation.status")
            && installed_coordinator.contains(
                "restart any still-pending reviewer slots from that same round by reusing their original `review_slot_id`s"
            )
            && installed_coordinator.contains(
                "Treat provider transport errors, exhausted reconnects, or other no-terminal-API reviewer exits as retryable dispatch failures"
            ),
        "installed coordinator should teach retrying pending reviewer slots after transport-style reviewer failures instead of waiting forever on a pending round"
    );
    assert!(
        installed_coordinator.contains(
            "If the artifact worker outcome returns `accepted_terminal_api = null`, reopen artifact execution for the same checkpoint"
        ),
        "installed coordinator should teach retrying artifact workers that exit without an accepted terminal API"
    );
    assert!(
        installed_coordinator.contains(
            "first resolve `<checkpoint_id>` with the read-only `.loopy/loopy.db` query above"
        )
            && installed_coordinator.contains(
                "re-evaluate the executable checkpoint state by rerunning the same read-only `.loopy/loopy.db` checkpoint query"
            ),
        "installed coordinator should require read-only checkpoint-id lookup before artifact dispatch and after artifact approval"
    );
    assert!(
        installed_coordinator.contains("blocking_issues")
            && installed_coordinator.contains("nonblocking_issues"),
        "installed coordinator should describe reviewer revision context in the redesigned structured issue terms"
    );
    assert!(
        installed_coordinator
            .contains("poll `show-loop --json` until the round stops being `pending`"),
        "installed coordinator should require show-loop polling until a review round reaches a terminal state"
    );
    assert!(
        installed_coordinator.contains("invocation_context.review_history.latest_result"),
        "installed coordinator should explain that reopened workers receive review revision guidance through invocation_context.review_history.latest_result"
    );
    assert!(
        installed_coordinator.contains("When artifact review rejects a submitted candidate commit"),
        "installed coordinator should explicitly reopen artifact execution after artifact review rejection"
    );
    assert!(
        !installed_coordinator.contains("issues and notes"),
        "installed coordinator should stop describing revision context as legacy issues-and-notes text"
    );
    assert!(
        installed_coordinator
            .contains("<bundle_bin> handoff-to-caller-finalize --loop-id <loop_id> --workspace <workspace_root>"),
        "installed coordinator should document the exact handoff-to-caller-finalize CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_skill.contains(
            "<bundle_bin> begin-caller-finalize --loop-id <loop_id> --workspace <workspace_root>"
        ),
        "installed SKILL.md should document the exact begin-caller-finalize CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_skill.contains(
            "<bundle_bin> block-caller-finalize --loop-id <loop_id> --strategy-summary <summary> --blocking-summary <summary> --human-question <question> --conflicting-files-json <json_array> --workspace <workspace_root>"
        ),
        "installed SKILL.md should document the exact block-caller-finalize CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_skill.contains(
            "<bundle_bin> finalize-success --loop-id <loop_id> --integration-summary-json <json_object> --workspace <workspace_root>"
        ),
        "installed SKILL.md should document the exact caller-owned finalize-success CLI form with explicit bundle_bin and workspace_root inputs"
    );
    assert!(
        installed_coordinator.contains(
            "<bundle_bin> finalize-failure --loop-id <loop_id> --failure-cause-type coordinator_failure --summary <summary> --workspace <workspace_root>"
        ),
        "installed coordinator should document the exact finalize-failure CLI form with explicit bundle_bin and workspace_root inputs"
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
