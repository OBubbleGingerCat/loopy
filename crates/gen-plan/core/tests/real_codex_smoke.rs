use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[test]
fn smoke_script_uses_the_installed_gen_plan_skill_entrypoint() -> Result<()> {
    let script = fs::read_to_string(repo_root().join("scripts/smoke-installed-gen-plan-codex.sh"))?;

    assert!(
        script.contains("CODEX_HOME_DIR/skills/loopy-gen-plan")
            || script.contains(".codex/skills/loopy-gen-plan"),
        "smoke script should install the real skill into an isolated CODEX_HOME"
    );
    assert!(
        script.contains("Use the `loopy:gen-plan` skill")
            || script.contains("Use the \\`loopy:gen-plan\\` skill")
            || script.contains("Skill name: `loopy:gen-plan`"),
        "smoke script should invoke the installed skill entrypoint"
    );
    assert!(
        script.contains("`loopy:gen-plan` is the skill name, not a shell command")
            || script.contains("\\`loopy:gen-plan\\` is the skill name, not a shell command"),
        "script should explicitly say the skill name is not a shell command"
    );
    assert!(
        script.contains("Do not try to execute `loopy:gen-plan` from the shell")
            || script.contains("Do not try to execute \\`loopy:gen-plan\\` from the shell"),
        "script should explicitly forbid shell execution of loopy:gen-plan"
    );
    assert!(
        script.contains("Treat the desired plan name, task type, and input path as semantic inputs")
            || script.contains("Treat the desired plan name, task type, and input path as semantic inputs.")
            || script.contains("Treat the desired plan name, task type, and input path as semantic inputs rather than a shell command"),
        "script should instruct Codex to use semantic inputs instead of a shell invocation"
    );
    assert!(
        script.contains("use the installed `bin/loopy-gen-plan` helper subcommands directly")
            || script
                .contains("use the installed \\`bin/loopy-gen-plan\\` helper subcommands directly"),
        "script should direct runtime helper usage to the installed binary subcommands"
    );
    assert!(
        !script.contains("$loopy:gen-plan"),
        "script should not include a shell-looking loopy:gen-plan preamble"
    );
    assert!(
        !script.contains("loopy:gen-plan --input draft.md --plan-name"),
        "script should not encourage loopy:gen-plan shell execution"
    );
    assert!(
        script.contains(
            "Do not inspect or print the installed `bin/loopy-gen-plan` ELF binary as text"
        ) || script.contains(
            "Do not inspect or print the installed \\`bin/loopy-gen-plan\\` ELF binary as text"
        ),
        "script should forbid inspecting the bundled ELF binary as text"
    );
    assert!(
        script.contains("Do not use `apply_patch` in this smoke.")
            || script.contains("Do not use \\`apply_patch\\` in this smoke."),
        "script should explicitly forbid apply_patch for this smoke"
    );
    assert!(
        script.contains("Write plan artifacts with shell file-writing commands")
            || script.contains("Write the plan artifacts with shell file-writing commands")
            || script.contains("Write plan artifacts with shell commands"),
        "script should require shell-based file writing for plan artifacts"
    );
    assert!(
        script.contains("Use `mkdir -p`, shell redirection, and `cat > file` style commands")
            || script.contains(
                "Use \\`mkdir -p\\`, shell redirection, and \\`cat > file\\` style commands"
            )
            || script
                .contains("Use shell redirection / mkdir / cat > file style commands instead."),
        "script should require mkdir plus shell redirection or cat for plan files"
    );
    assert!(
        script.contains("--plan-name rust-cli-todo")
            || script.contains("Desired plan name: `$plan_name`")
            || script.contains("Desired plan name: \\`$plan_name\\`"),
        "script should describe the rust-cli-todo plan name"
    );
    assert!(
        script.contains("--plan-name fastapi-notes-api")
            || script.contains("Desired plan name: `$plan_name`")
            || script.contains("Desired plan name: \\`$plan_name\\`"),
        "script should cover a second auto-mode prompt"
    );
    assert!(
        script.contains("--plan-name csv-export-rust-report")
            || script.contains("Desired plan name: `$plan_name`")
            || script.contains("Desired plan name: \\`$plan_name\\`"),
        "script should cover a seeded-repo prompt"
    );
    assert!(
        script.contains("rust-cli-todo")
            && script.contains("fastapi-notes-api")
            && script.contains("csv-export-rust-report"),
        "script should preserve all three named smoke cases"
    );
    assert!(
        script.contains("install-gen-plan-skill.sh\" --target codex")
            || script.contains("install-gen-plan-skill.sh --target codex"),
        "script should install the staged skill via the codex target"
    );
    assert!(
        script.contains("Use auto mode."),
        "script should opt into auto mode explicitly"
    );
    assert!(
        script.contains("Require real reviewer behavior only.")
            || script.contains("Use real reviewer behavior only."),
        "script should require real reviewer behavior in the prompt contract"
    );
    assert!(
        script.contains("reviewer_role_id=mock"),
        "script should explicitly reject mock reviewer role ids"
    );
    assert!(
        script.contains("validate_no_mock_gate_artifacts"),
        "script should validate mock markers against persisted gate artifacts"
    );
    assert!(
        script.contains("gate-runs") && script.contains("last-message.json"),
        "script should inspect gate last-message artifacts rather than only the top-level log"
    );
    assert!(
        !script.contains("grep -Fq \"$marker\" \"$log_file\""),
        "script should not reject runs based on prompt text echoed into the top-level log"
    );
    assert!(
        script.contains("Task 4 uses deterministic mock reviewer execution."),
        "script should explicitly reject the deterministic mock rationale"
    );
    assert!(
        script.contains("Mock leaf review requires a revision.")
            && script.contains("Mock frontier review invalidated a leaf.")
            && script.contains("Mock frontier review found no child leaves to invalidate."),
        "script should explicitly reject the deterministic mock summaries"
    );
    assert!(
        script.contains("continue with the installed `codex_default` reviewer instructions")
            || script
                .contains("continue with the installed \\`codex_default\\` reviewer instructions"),
        "script should instruct Codex to fall back to the installed codex_default reviewer instructions"
    );
    assert!(
        script.contains("always pass `--parent-relative-path`")
            || script.contains("always pass \\`--parent-relative-path\\`"),
        "script should require parent-relative-path for child node registration"
    );
    assert!(
        script.contains("Treat the installed runtime APIs as the only authoritative source of plan runtime state.")
            || script.contains("Treat the installed runtime APIs as the only authoritative source of plan runtime state"),
        "script should require planner to treat runtime APIs as the only authoritative state source"
    );
    assert!(
        script.contains("A plan is not established until installed `ensure-plan` or `open-plan` succeeds.")
            || script.contains("A plan is not established until installed \\`ensure-plan\\` or \\`open-plan\\` succeeds."),
        "script should require ensure-plan/open-plan before tracked plan work"
    );
    assert!(
        script.contains("A node is not tracked until installed `ensure-node-id` succeeds.")
            || script.contains("A node is not tracked until installed \\`ensure-node-id\\` succeeds."),
        "script should require ensure-node-id before treating nodes as tracked"
    );
    assert!(
        script.contains(
            "A review gate has not happened unless installed `run-leaf-review-gate` or `run-frontier-review-gate` returns a valid gate result."
        ) || script.contains(
            "A review gate has not happened unless installed \\`run-leaf-review-gate\\` or \\`run-frontier-review-gate\\` returns a valid gate result."
        ),
        "script should forbid treating self-review as a gate substitute"
    );
    assert!(
        script.contains("Always invoke installed runtime helpers against the project workspace root, not a nested `.loopy/plans/` directory.")
            || script.contains("Always invoke installed runtime helpers against the project workspace root, not a nested \\`.loopy/plans/\\` directory."),
        "script should require runtime helpers to use the project workspace root"
    );
    assert!(
        script.contains("Do not self-review, hand-wave, or write free-text reviewer verdicts in place of runtime gate output.")
            || script.contains("Do not self-review, hand-wave, or write free-text reviewer verdicts in place of runtime gate output"),
        "script should explicitly forbid bypassing runtime review gates"
    );
    assert!(
        script.contains("Do not inspect `.loopy/loopy.db` directly, including broad file-dump commands that would read it as text.")
            || script.contains("Do not inspect \\`.loopy/loopy.db\\` directly, including broad file-dump commands that would read it as text."),
        "script should explicitly forbid direct loopy.db reads"
    );
    assert!(
        script.contains("For this smoke, if packaging or crate metadata needs a license decision, use `MIT` as an explicitly user-approved default.")
            || script.contains("For this smoke, if packaging or crate metadata needs a license decision, use \\`MIT\\` as an explicitly user-approved default."),
        "script should provide an explicit user-approved license default for auto-mode packaging leaves"
    );
    assert!(
        script.contains("Use installed `ensure-plan`, then installed `open-plan`, before continuing with tracked plan work.")
            || script.contains("Use installed \\`ensure-plan\\`, then installed \\`open-plan\\`, before continuing with tracked plan work."),
        "script should require both ensure-plan and open-plan in the smoke workflow"
    );
    assert!(
        script.contains("If installed `ensure-plan`, `open-plan`, or `ensure-node-id` fails because of request construction or missing prerequisite runtime state, use the returned runtime error plus the current plan tree/runtime state to repair the runtime call sequence.")
            || script.contains("If installed \\`ensure-plan\\`, \\`open-plan\\`, or \\`ensure-node-id\\` fails because of request construction or missing prerequisite runtime state, use the returned runtime error plus the current plan tree/runtime state to repair the runtime call sequence."),
        "script should allow controlled recovery for recoverable runtime API failures"
    );
    assert!(
        script.contains("During runtime-call recovery for `ensure-plan`, `open-plan`, or `ensure-node-id`, do not change plan content.")
            || script.contains("During runtime-call recovery for \\`ensure-plan\\`, \\`open-plan\\`, or \\`ensure-node-id\\`, do not change plan content."),
        "script should forbid plan-content edits during runtime-call recovery"
    );
    assert!(
        script.contains("Do not blindly guess parameters or keep replaying the same class of runtime error without new runtime evidence or relevant state changes.")
            || script.contains("Do not blindly guess parameters or keep replaying the same class of runtime error without new runtime evidence or relevant state changes"),
        "script should forbid blind runtime API guessing loops"
    );
    assert!(
        script.contains("parent node's self-description markdown path")
            || script.contains("parent node’s self-description markdown path"),
        "script should explain what --parent-relative-path must point to"
    );
    assert!(
        script.contains("Do not run leaf review on non-leaf parent nodes.")
            || script.contains("Do not run leaf review on non-leaf parent nodes"),
        "script should forbid leaf review on non-leaf parent nodes"
    );
    assert!(
        script.contains("Never mutate `.loopy/loopy.db` directly.")
            || script.contains("Never mutate \\`.loopy/loopy.db\\` directly."),
        "script should explicitly forbid direct loopy.db mutation"
    );
    assert!(
        script.contains("Never read `.loopy/loopy.db` directly as a planning aid or recovery shortcut.")
            || script.contains("Never read \\`.loopy/loopy.db\\` directly as a planning aid or recovery shortcut."),
        "script should explicitly forbid direct loopy.db reads as runtime shortcuts"
    );
    assert!(
        script.contains("fail rather than patching the DB"),
        "script should require the run to fail instead of repairing inconsistent runtime metadata"
    );
    assert!(
        script.contains(
            "If installed `run-leaf-review-gate` or `run-frontier-review-gate` fails to launch, times out, fails to write the expected runtime artifact, or fails to return parseable valid output, immediately retry the same gate call up to 5 times without changing files, ids, or arguments."
        ) || script.contains(
            "If installed \\`run-leaf-review-gate\\` or \\`run-frontier-review-gate\\` fails to launch, times out, fails to write the expected runtime artifact, or fails to return parseable valid output, immediately retry the same gate call up to 5 times without changing files, ids, or arguments."
        ),
        "script should encode the gate invocation retry contract"
    );
    assert!(
        script.contains("If all 5 immediate retries fail for the same gate call, stop and surface the combined failure instead of bypassing the gate.")
            || script.contains("If all 5 immediate retries fail for the same gate call, stop and surface the combined failure instead of bypassing the gate"),
        "script should stop instead of bypassing a repeatedly failing gate"
    );
    assert!(
        script.contains("If a gate call succeeds and returns review issues, revise the plan and then submit a new gate call; do not treat review issues as a retry case.")
            || script.contains("If a gate call succeeds and returns review issues, revise the plan and then submit a new gate call; do not treat review issues as a retry case"),
        "script should distinguish review issues from invocation-layer retry cases"
    );
    assert!(
        script.contains("LOOPY_SMOKE_STRICT_VALIDATION"),
        "script should expose strict validation control"
    );
    assert!(
        script.contains("strict validation") || script.contains("STRICT_VALIDATION"),
        "script should describe the strict validation mode"
    );
    assert!(
        script.contains("parent_node_id IS NOT NULL")
            || script.contains("parent_node_id is not null")
            || script.contains("non-flat node metadata"),
        "script should validate that runtime metadata is not flat"
    );
    assert!(
        script.contains("reviewer_role_id <> 'mock'")
            || script.contains("reviewer_role_id != 'mock'")
            || script.contains("non-mock gate usage"),
        "script should validate that real reviewer roles were persisted"
    );
    assert!(
        script.contains("validate_runtime_api_transcript_usage"),
        "script should validate actual runtime API usage from exec transcripts"
    );
    assert!(
        script.contains("strict validation missing required runtime API")
            && script.contains("strict validation saw runtime APIs out of order")
            && script.contains("validate_runtime_api_transcript_usage"),
        "script should enforce required runtime API calls and ordering from exec transcripts"
    );
    assert!(
        script.contains(".loopy/loopy.db")
            && script.contains("sqlite")
            && script.contains("update")
            && script.contains("insert")
            && script.contains("delete"),
        "script should reject direct sqlite write attempts against the runtime DB"
    );
    assert!(
        script.contains("detected direct sqlite read attempt against .loopy/loopy.db")
            || script.contains("detected indirect text inspection of .loopy/loopy.db"),
        "script should reject direct sqlite read attempts against the runtime DB"
    );
    assert!(
        !script.contains("mock_leaf_reviewer") && !script.contains("mock_frontier_reviewer"),
        "script should rely on the installed real reviewer defaults instead of mock reviewers"
    );
    assert!(
        script.contains("ARTIFACT_ROOT=$RUN_ROOT"),
        "script should report the smoke artifact root"
    );
    assert!(
        script.contains("RESULT_SOURCE=direct"),
        "script should report the direct installed-skill execution path"
    );

    Ok(())
}

#[test]
#[ignore = "requires a real codex exec run against the installed gen-plan skill"]
fn installed_bundle_real_codex_auto_mode_smoke_path_succeeds() -> Result<()> {
    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root())
        .output()
        .context("failed to run real codex smoke script")?;
    if !output.status.success() {
        bail!(
            "real codex smoke script failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[test]
fn smoke_script_preserves_artifacts_for_all_auto_mode_cases() -> Result<()> {
    let repo_root = repo_root();
    let temp = support::workspace()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "success")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-success");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_STRICT_VALIDATION", "0")
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with fake direct codex")?;
    if !output.status.success() {
        bail!("fake direct smoke failed\n{}", combined_output(&output));
    }

    let combined = combined_output(&output);
    assert!(
        combined.contains(&format!("ARTIFACT_ROOT={}", run_root.display())),
        "expected artifact root marker in output:\n{combined}"
    );
    assert!(
        combined.contains("RESULT_SOURCE=direct"),
        "expected direct result marker in output:\n{combined}"
    );

    for case_name in [
        "rust-cli-todo",
        "fastapi-notes-api",
        "csv-export-rust-report",
    ] {
        let workspace = run_root.join("workspaces").join(case_name);
        let plan_root = workspace.join(".loopy/plans").join(case_name);
        assert!(
            workspace.is_dir(),
            "expected workspace at {}",
            workspace.display()
        );
        assert!(
            plan_root.is_dir(),
            "expected plan root at {}",
            plan_root.display()
        );
        assert!(
            plan_root.join(format!("{case_name}.md")).is_file(),
            "expected generated markdown node for {case_name}"
        );
        assert!(
            run_root
                .join("logs")
                .join(format!("{case_name}.log"))
                .is_file(),
            "expected log file for {case_name}"
        );
        assert!(
            run_root
                .join("prompts")
                .join(format!("{case_name}.prompt.md"))
                .is_file(),
            "expected prompt file for {case_name}"
        );
        assert!(
            run_root
                .join("last-messages")
                .join(format!("{case_name}.json"))
                .is_file(),
            "expected last-message capture for {case_name}"
        );
    }

    Ok(())
}

#[test]
fn smoke_script_rejects_unknown_case_filter() -> Result<()> {
    let repo_root = repo_root();
    let temp = support::workspace()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "success")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-unknown-filter");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("LOOPY_SMOKE_CASE_FILTER", "does-not-exist")
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with invalid case filter")?;

    assert!(
        !output.status.success(),
        "invalid case filter should fail\n{}",
        combined_output(&output)
    );

    let combined = combined_output(&output);
    assert!(
        combined.contains("unknown smoke case in LOOPY_SMOKE_CASE_FILTER"),
        "expected unknown-case error in output:\n{combined}"
    );

    Ok(())
}

#[test]
fn smoke_script_strict_validation_passes_with_fake_strict_artifacts() -> Result<()> {
    let repo_root = repo_root();
    let temp = support::workspace()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "strict_success")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-strict-success");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("LOOPY_SMOKE_CASE_FILTER", "rust-cli-todo")
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with strict fake codex")?;

    if !output.status.success() {
        bail!("strict fake smoke failed\n{}", combined_output(&output));
    }

    let combined = combined_output(&output);
    assert!(
        combined.contains("RESULT_SOURCE=direct"),
        "expected direct result marker in output:\n{combined}"
    );

    Ok(())
}

#[test]
fn smoke_script_strict_validation_accepts_direct_helper_paths() -> Result<()> {
    let repo_root = repo_root();
    let temp = support::workspace()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "strict_success_direct_path")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-strict-success-direct-path");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("LOOPY_SMOKE_CASE_FILTER", "rust-cli-todo")
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with direct-path strict fake codex")?;

    if !output.status.success() {
        bail!(
            "strict direct-path fake smoke failed\n{}",
            combined_output(&output)
        );
    }

    Ok(())
}

#[test]
fn smoke_script_rejects_direct_db_write_attempts_from_exec_transcript() -> Result<()> {
    let repo_root = repo_root();
    let temp = support::workspace()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "strict_direct_db_write")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-strict-db-write");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("LOOPY_SMOKE_CASE_FILTER", "rust-cli-todo")
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with fake direct-db-write transcript")?;

    assert!(
        !output.status.success(),
        "strict validation should reject direct DB writes\n{}",
        combined_output(&output)
    );

    let combined = combined_output(&output);
    assert!(
        combined.contains("detected direct sqlite write attempt against .loopy/loopy.db"),
        "expected direct DB write validation error in output:\n{combined}"
    );

    Ok(())
}

#[test]
fn smoke_script_rejects_direct_db_read_attempts_from_exec_transcript() -> Result<()> {
    let repo_root = repo_root();
    let temp = support::workspace()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "strict_direct_db_read")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-strict-db-read");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-gen-plan-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("LOOPY_SMOKE_CASE_FILTER", "rust-cli-todo")
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with fake direct-db-read transcript")?;

    assert!(
        !output.status.success(),
        "strict validation should reject direct DB reads\n{}",
        combined_output(&output)
    );

    let combined = combined_output(&output);
    assert!(
        combined.contains("detected direct sqlite read attempt against .loopy/loopy.db")
            || combined.contains("detected indirect text inspection of .loopy/loopy.db"),
        "expected direct DB read validation error in output:\n{combined}"
    );

    Ok(())
}

fn repo_root() -> &'static Path {
    static REPO_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REPO_ROOT.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("repo root should resolve")
    })
}

fn write_fake_codex(bin_path: &Path, mode: &str) -> Result<()> {
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

mode="{mode}"
workspace=""
output_file=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    exec)
      shift
      ;;
    -C)
      workspace="$2"
      shift 2
      ;;
    -o|--output-last-message)
      output_file="$2"
      shift 2
      ;;
    -c|-m|--color|--add-dir)
      shift 2
      ;;
    --full-auto|--skip-git-repo-check)
      shift
      ;;
    -)
      shift
      ;;
    *)
      shift
      ;;
  esac
done

prompt="$(cat)"
plan_name="$(printf '%s\n' "$prompt" | grep -m1 '^- Desired plan name: `' | cut -d'`' -f2)"

if [[ -z "$plan_name" ]]; then
  plan_name="$(printf '%s' "$prompt" | grep -oE -- '--plan-name [^` ]+' | head -n1 | awk '{{print $2}}')"
fi

    if [[ "$mode" == "success" || "$mode" == "strict_success" || "$mode" == "strict_success_direct_path" || "$mode" == "strict_direct_db_write" || "$mode" == "strict_direct_db_read" ]]; then
      mkdir -p "$workspace/.loopy/plans/$plan_name"
      cat >"$workspace/.loopy/plans/$plan_name/$plan_name.md" <<EOF
# $plan_name

- generated by fake codex
EOF
      if [[ "$mode" == "strict_success" || "$mode" == "strict_success_direct_path" || "$mode" == "strict_direct_db_write" || "$mode" == "strict_direct_db_read" ]]; then
        mkdir -p "$workspace/.loopy/gate-runs/leaf-1" "$workspace/.loopy/gate-runs/frontier-1"
        python3 - "$workspace" "$plan_name" <<'PY'
import pathlib
import sqlite3
import sys

workspace = pathlib.Path(sys.argv[1])
plan_name = sys.argv[2]
db_path = workspace / ".loopy" / "loopy.db"
db_path.parent.mkdir(parents=True, exist_ok=True)
con = sqlite3.connect(db_path)
con.executescript(
    """
    CREATE TABLE GEN_PLAN__plans (
      plan_id TEXT PRIMARY KEY,
      workspace_root TEXT,
      plan_name TEXT,
      plan_root TEXT,
      task_type TEXT,
      plan_status TEXT,
      created_at TEXT,
      updated_at TEXT
    );
    CREATE TABLE GEN_PLAN__nodes (
      plan_id TEXT,
      node_id TEXT,
      relative_path TEXT,
      node_name TEXT,
      parent_node_id TEXT,
      created_at TEXT,
      updated_at TEXT
    );
    CREATE TABLE GEN_PLAN__leaf_gate_runs (
      leaf_gate_run_id TEXT,
      plan_id TEXT,
      node_id TEXT,
      reviewer_role_id TEXT,
      planner_mode TEXT,
      passed INTEGER,
      verdict TEXT,
      summary TEXT,
      issues_json TEXT,
      created_at TEXT
    );
    CREATE TABLE GEN_PLAN__frontier_gate_runs (
      frontier_gate_run_id TEXT,
      plan_id TEXT,
      parent_node_id TEXT,
      reviewer_role_id TEXT,
      planner_mode TEXT,
      passed INTEGER,
      verdict TEXT,
      summary TEXT,
      issues_json TEXT,
      invalidated_leaf_node_ids_json TEXT,
      created_at TEXT
    );
    """
)
con.execute(
    "INSERT INTO GEN_PLAN__plans VALUES (?, ?, ?, ?, ?, ?, '', '')",
    (
        "plan-1",
        str(workspace),
        plan_name,
        f"./.loopy/plans/{{plan_name}}",
        "coding-task",
        "ready",
    ),
)
con.execute(
    "INSERT INTO GEN_PLAN__nodes VALUES (?, ?, ?, ?, ?, '', '')",
    ("plan-1", "parent-1", f"{{plan_name}}.md", plan_name, None),
)
con.execute(
    "INSERT INTO GEN_PLAN__nodes VALUES (?, ?, ?, ?, ?, '', '')",
    ("plan-1", "leaf-1", f"{{plan_name}}/leaf.md", "leaf", "parent-1"),
)
con.execute(
    "INSERT INTO GEN_PLAN__leaf_gate_runs VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '')",
    (
        "leaf-run-1",
        "plan-1",
        "leaf-1",
        "codex_default",
        "auto",
        1,
        "approved_as_leaf",
        "ok",
        "[]",
    ),
)
con.execute(
    "INSERT INTO GEN_PLAN__frontier_gate_runs VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '')",
    (
        "frontier-run-1",
        "plan-1",
        "parent-1",
        "codex_default",
        "auto",
        1,
        "approved_frontier",
        "ok",
        "[]",
        "[]",
    ),
)
con.commit()
PY
        cat >"$workspace/.loopy/gate-runs/leaf-1/last-message.json" <<EOF
{{"reviewer_role_id":"codex_default","summary":"ok"}}
EOF
        cat >"$workspace/.loopy/gate-runs/frontier-1/last-message.json" <<EOF
{{"reviewer_role_id":"codex_default","summary":"ok"}}
EOF
        if [[ "$mode" == "strict_success_direct_path" ]]; then
          cat <<'EOF'
exec
/bin/bash -lc '/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan --workspace . ensure-plan --plan-name strict-plan --task-type coding-task --project-directory .'
exec
/bin/bash -lc '/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan --workspace . open-plan --plan-name strict-plan'
exec
/bin/bash -lc '/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan --workspace . ensure-node-id --plan-id plan-1 --relative-path strict-plan/leaf.md --parent-relative-path strict-plan.md'
 succeeded in 0ms:
exec
/bin/bash -lc '/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan --workspace . run-leaf-review-gate --plan-id plan-1 --node-id leaf-1 --planner-mode auto'
 succeeded in 0ms:
exec
/bin/bash -lc '/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan --workspace . run-frontier-review-gate --plan-id plan-1 --parent-node-id parent-1 --planner-mode auto'
 succeeded in 0ms:
 succeeded in 0ms:
 succeeded in 0ms:
EOF
        else
        cat <<'EOF'
exec
/bin/bash -lc 'bin=/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan
"$bin" ensure-plan --workspace . --plan-name strict-plan --task-type coding-task --project-directory .' 
 succeeded in 0ms:
exec
/bin/bash -lc 'bin=/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan
"$bin" open-plan --workspace . --plan-name strict-plan'
 succeeded in 0ms:
exec
/bin/bash -lc 'bin=/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan
"$bin" ensure-node-id --workspace . --plan-id plan-1 --relative-path strict-plan/leaf.md --parent-relative-path strict-plan.md'
 succeeded in 0ms:
exec
/bin/bash -lc 'bin=/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan
"$bin" run-leaf-review-gate --workspace . --plan-id plan-1 --node-id leaf-1 --planner-mode auto'
 succeeded in 0ms:
exec
/bin/bash -lc 'bin=/tmp/fake-codex-home/.codex/skills/loopy-gen-plan/bin/loopy-gen-plan
"$bin" run-frontier-review-gate --workspace . --plan-id plan-1 --parent-node-id parent-1 --planner-mode auto'
 succeeded in 0ms:
EOF
        fi
      fi
      cat >"$output_file" <<EOF
{{"plan_name":"$plan_name","status":"ok"}}
EOF
      if [[ "$mode" == "strict_direct_db_write" ]]; then
        cat <<'EOF'
exec
bash -lc "python3 - <<'PY'
import sqlite3
connection = sqlite3.connect('.loopy/loopy.db')
connection.execute(\"update GEN_PLAN__plans set plan_status = 'active'\")
connection.commit()
PY"
 succeeded in 0ms:
EOF
      fi
      if [[ "$mode" == "strict_direct_db_read" ]]; then
        cat <<'EOF'
exec
bash -lc "find .loopy -maxdepth 4 -type f | sort | xargs -I{{}} sh -c 'echo \"--- {{}} ---\"; sed -n \"1,40p\" \"{{}}\"'"
 succeeded in 0ms:
--- .loopy/loopy.db ---
SQLite format 3
EOF
      fi
      echo "fake-codex-direct-path"
      exit 0
    fi

echo "fake codex failure for $plan_name" >&2
exit 1
"#
    );
    fs::write(bin_path, script)?;
    let mut perms = fs::metadata(bin_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(bin_path, perms)?;
    Ok(())
}

fn write_fake_codex_home(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    fs::write(
        root.join("config.toml"),
        r#"model_provider = "OpenAI"
model = "gpt-5.4"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.OpenAI]
name = "OpenAI"
base_url = "https://rust.cat"
wire_api = "responses"
requires_openai_auth = true
"#,
    )?;
    fs::write(root.join("auth.json"), r#"{"fake":"auth"}"#)?;
    Ok(())
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
mod support;
