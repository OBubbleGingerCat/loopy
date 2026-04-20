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
            || script.contains("$loopy:gen-plan"),
        "smoke script should invoke the installed skill entrypoint"
    );
    assert!(
        script.contains("Treat `loopy:gen-plan` as the installed entrypoint")
            || script.contains("Treat the skill name `loopy:gen-plan` as the installed entrypoint")
            || script.contains("Treat the skill name \\`loopy:gen-plan\\` as the installed entrypoint"),
        "script should explicitly treat the installed skill name as the entrypoint"
    );
    assert!(
        script.contains("Do not inspect or print the installed `bin/loopy-gen-plan` ELF binary as text")
            || script.contains("Do not inspect or print the installed \\`bin/loopy-gen-plan\\` ELF binary as text"),
        "script should forbid inspecting the bundled ELF binary as text"
    );
    assert!(
        script.contains("--plan-name rust-cli-todo"),
        "script should drive a named auto-mode plan"
    );
    assert!(
        script.contains("--plan-name fastapi-notes-api"),
        "script should cover a second auto-mode prompt"
    );
    assert!(
        script.contains("--plan-name csv-export-rust-report"),
        "script should cover a seeded-repo prompt"
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
            || script.contains("continue with the installed \\`codex_default\\` reviewer instructions"),
        "script should instruct Codex to fall back to the installed codex_default reviewer instructions"
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
plan_name="$(printf '%s' "$prompt" | grep -oE -- '--plan-name [^` ]+' | head -n1 | awk '{{print $2}}')"

if [[ "$mode" == "success" ]]; then
  mkdir -p "$workspace/.loopy/plans/$plan_name"
  cat >"$workspace/.loopy/plans/$plan_name/$plan_name.md" <<EOF
# $plan_name

- generated by fake codex
EOF
  cat >"$output_file" <<EOF
{{"plan_name":"$plan_name","status":"ok"}}
EOF
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
