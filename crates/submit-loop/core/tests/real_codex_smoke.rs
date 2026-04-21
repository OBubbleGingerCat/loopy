mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

#[test]
fn smoke_script_uses_the_installed_skill_entrypoint_instead_of_inlining_bundle_prompts()
-> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let script = fs::read_to_string(repo_root.join("scripts/smoke-installed-bundle-codex.sh"))?;

    assert!(
        script.contains("CODEX_HOME_DIR/skills/loopy-submit-loop")
            || script.contains(".codex/skills/loopy-submit-loop"),
        "smoke script should stage the installed bundle as a real Codex skill"
    );
    assert!(
        script.contains("Use the \\`loopy:submit-loop\\` skill")
            || script.contains("Use the `loopy:submit-loop` skill")
            || script.contains("\\$loopy:submit-loop"),
        "smoke script should invoke the installed skill entrypoint"
    );
    assert!(
        !script.contains("$(cat \"$INSTALL_ROOT/SKILL.md\")"),
        "smoke script should not inline SKILL.md into a custom prompt"
    );
    assert!(
        !script.contains("$(cat \"$INSTALL_ROOT/roles/coordinator.md\")"),
        "smoke script should not inline roles/coordinator.md into a custom prompt"
    );
    assert!(
        script.contains("\"task_type\": \"coding-task\""),
        "smoke script should send a caller request object with task_type"
    );
    assert!(
        script.contains("$INSTALL_ROOT/coordinator.md"),
        "smoke script should validate the top-level coordinator asset"
    );
    assert!(
        !script.contains("$INSTALL_ROOT/roles/coordinator.md"),
        "smoke script should stop depending on the legacy coordinator path"
    );
    assert!(
        !script.contains("open-loop --summary smoke-blocked"),
        "smoke script should not dictate the coordinator CLI sequence itself"
    );
    assert!(
        script.contains("cp \"$SOURCE_CODEX_HOME/config.toml\" \"$CODEX_HOME_DIR/config.toml\""),
        "smoke script should bootstrap the isolated CODEX_HOME with the caller Codex config"
    );
    assert!(
        !script.contains("-m gpt-5.4"),
        "smoke script should let the caller model come from the staged Codex config instead of hard-coding gpt-5.4"
    );
    assert!(
        script.contains("cp \"$SOURCE_CODEX_HOME/auth.json\" \"$CODEX_HOME_DIR/auth.json\""),
        "smoke script should bootstrap the isolated CODEX_HOME with the caller Codex auth"
    );
    assert!(
        script.contains("--add-dir \"$CODEX_HOME_DIR\""),
        "smoke script should allow the isolated CODEX_HOME root to stay writable during the outer codex exec"
    );
    assert!(
        script.contains("install-submit-loop-skill.sh\" --target codex")
            || script.contains("install-submit-loop-skill.sh --target codex"),
        "smoke script should install the bundle into the isolated CODEX_HOME via --target codex"
    );
    assert!(
        !script.contains("INSTALL_ROOT=\"$WORKSPACE/.loopy/installed-skills/loopy-submit-loop\""),
        "smoke script should not stage the installed bundle under the workspace runtime directory"
    );
    assert!(
        script.contains("LOOPY_SMOKE_ALLOW_TRANSPORT_FALLBACK"),
        "smoke script should require explicit opt-in before using the transport fallback"
    );
    assert!(
        script.contains("payload[\"failure_cause_type\"] == \"worker_blocked\""),
        "smoke script should validate the stable real-Codex blocked-case failure cause as worker_blocked"
    );
    assert!(
        script.contains("RESULT_SOURCE=direct"),
        "smoke script should emit direct-path evidence for acceptance runs"
    );
    assert!(
        !script.contains("sqlite3.connect"),
        "smoke script must not inspect the runtime sqlite database directly"
    );
    assert!(
        !script.contains("CORE__events"),
        "smoke script transport handling must not query runtime event tables directly"
    );

    Ok(())
}

#[test]
fn real_codex_suite_script_runs_multiple_cases_without_mock_roles() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let script = fs::read_to_string(repo_root.join("scripts/real-submit-loop-suite-codex.sh"))?;

    assert!(
        script.contains("$loopy:submit-loop")
            || script.contains("\\$loopy:submit-loop")
            || script.contains("Use the `loopy:submit-loop` skill"),
        "suite script should invoke the installed loopy:submit-loop skill entrypoint"
    );
    assert!(
        script.contains("case-blocked-smoke")
            && script.contains("case-readme-append")
            && script.contains("case-proof-file"),
        "suite script should cover multiple distinct real-Codex propositions"
    );
    assert!(
        !script.contains("mock_planner")
            && !script.contains("mock_implementer")
            && !script.contains("\"mock\"")
            && !script.contains("executor = \"mock"),
        "suite script must not depend on mock roles or mock executors"
    );
    assert!(
        script.contains("codex exec")
            && script.contains("install-submit-loop-skill.sh")
            && script.contains("RESULT_SOURCE=direct"),
        "suite script should install the bundle and drive it through real codex exec runs with auditable output"
    );
    assert!(
        script.contains("--add-dir \"$CODEX_HOME_DIR\""),
        "suite script should keep the isolated CODEX_HOME writable during the outer codex exec"
    );

    Ok(())
}

#[test]
#[ignore = "requires a real codex exec run against the installed bundle"]
fn installed_bundle_real_codex_smoke_path_returns_a_failure_result() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let output = Command::new("bash")
        .arg("scripts/smoke-installed-bundle-codex.sh")
        .current_dir(repo_root)
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

fn write_fake_codex(bin_path: &Path, mode: &str) -> Result<()> {
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

mode="{mode}"
output_file=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    exec)
      shift
      ;;
    -o|--output-last-message)
      output_file="$2"
      shift 2
      ;;
    -m|-c|-C|--color)
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

cat >/dev/null || true

if [[ "$mode" == "success" ]]; then
  cat >"$output_file" <<'EOF'
{{"failure_cause_type":"worker_blocked","last_stable_context":{{"base_commit_sha":"seed","worktree_branch":"loopy-loop-test","worktree_label":"submit-test"}},"loop_id":"loop-test","phase_at_failure":"planning","result_generated_at":"2026-04-09T00:00:00Z","source_event_id":8,"status":"failure","summary":"No plannable repository work is present","worktree_ref":{{"branch":"loopy-loop-test","label":"submit-test","path":".loopy/worktrees/submit-test"}}}}
EOF
  echo "fake-codex-direct-path"
  exit 0
fi

echo "ERROR: stream disconnected before completion: error sending request for url (https://rust.cat/responses)" >&2
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

#[test]
fn smoke_script_preserves_auditable_artifacts_when_direct_path_succeeds() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let temp = tempfile::tempdir()?;
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
        .arg("scripts/smoke-installed-bundle-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
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
    assert!(run_root.join("workspace").is_dir());
    assert!(run_root.join("codex-last-message.json").is_file());
    assert!(run_root.join("logs/attempt-1.combined.log").is_file());

    Ok(())
}

#[test]
fn smoke_script_preserves_auditable_artifacts_when_direct_path_fails() -> Result<()> {
    let repo_root = crate::support::repo_root().as_path();
    let temp = tempfile::tempdir()?;
    let fake_bin_dir = temp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir)?;
    write_fake_codex(&fake_bin_dir.join("codex"), "fail")?;

    let source_codex_home = temp.path().join("source-codex-home");
    write_fake_codex_home(&source_codex_home)?;

    let run_root = temp.path().join("run-fail");
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("bash")
        .arg("scripts/smoke-installed-bundle-codex.sh")
        .current_dir(repo_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CODEX_HOME", &source_codex_home)
        .env("LOOPY_SMOKE_RUN_ROOT", &run_root)
        .env("PATH", path)
        .output()
        .context("failed to run smoke script with fake failing codex")?;
    if output.status.success() {
        bail!(
            "fake failing smoke unexpectedly succeeded\n{}",
            combined_output(&output)
        );
    }

    let combined = combined_output(&output);
    assert!(
        combined.contains(&format!("ARTIFACT_ROOT={}", run_root.display())),
        "expected artifact root marker in output:\n{combined}"
    );
    assert!(
        combined.contains("transport fallback disabled"),
        "expected direct-only failure note in output:\n{combined}"
    );
    assert!(run_root.join("workspace").is_dir());
    assert!(run_root.join("logs/attempt-1.combined.log").is_file());
    assert!(run_root.join("logs/attempt-3.combined.log").is_file());

    Ok(())
}

#[test]
fn mirrored_gitdir_fallback_sequence_can_create_a_private_worktree() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let repo_path = repo.path();

    let init = Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(repo_path)
        .output()
        .context("failed to init git repo")?;
    if !init.status.success() {
        bail!("git init failed\n{}", combined_output(&init));
    }

    for (key, value) in [("user.name", "Codex"), ("user.email", "codex@example.com")] {
        let output = Command::new("git")
            .args(["config", key, value])
            .current_dir(repo_path)
            .output()
            .with_context(|| format!("failed to configure git {key}"))?;
        if !output.status.success() {
            bail!("git config failed\n{}", combined_output(&output));
        }
    }

    fs::write(repo_path.join("README.md"), "seed\n")?;
    let add = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(repo_path)
        .output()
        .context("failed to git add seed file")?;
    if !add.status.success() {
        bail!("git add failed\n{}", combined_output(&add));
    }
    let commit = Command::new("git")
        .args(["commit", "-m", "seed"])
        .current_dir(repo_path)
        .output()
        .context("failed to git commit seed file")?;
    if !commit.status.success() {
        bail!("git commit failed\n{}", combined_output(&commit));
    }

    let mirror = repo_path.join(".loopy/git-common-submit-test");
    let worktree = repo_path.join(".loopy/worktrees/submit-test");
    fs::create_dir_all(&mirror)?;
    fs::create_dir_all(worktree.parent().expect("worktree parent"))?;

    let copy_git = Command::new("cp")
        .args(["-a", ".git/.", mirror.to_string_lossy().as_ref()])
        .current_dir(repo_path)
        .output()
        .context("failed to copy primary gitdir into writable mirror")?;
    if !copy_git.status.success() {
        bail!("gitdir mirror copy failed\n{}", combined_output(&copy_git));
    }

    let add_worktree = Command::new("git")
        .args([
            format!("--git-dir={}", mirror.display()).as_str(),
            format!("--work-tree={}", repo_path.display()).as_str(),
            "worktree",
            "add",
            "-b",
            "loopy-loop-test",
            worktree.to_string_lossy().as_ref(),
            "HEAD",
        ])
        .current_dir(repo_path)
        .output()
        .context("failed to add mirrored-gitdir worktree")?;
    if !add_worktree.status.success() {
        bail!(
            "mirrored-gitdir worktree add failed\n{}",
            combined_output(&add_worktree)
        );
    }

    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&worktree)
        .output()
        .context("failed to inspect mirrored worktree branch")?;
    if !branch.status.success() {
        bail!("git rev-parse failed\n{}", combined_output(&branch));
    }

    assert!(worktree.join(".git").exists());
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "loopy-loop-test"
    );

    Ok(())
}
