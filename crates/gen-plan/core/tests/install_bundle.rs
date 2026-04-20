mod support;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

#[test]
fn install_script_copies_required_gen_plan_assets_with_positional_path() -> Result<()> {
    let workspace = support::workspace()?;
    support::assert_dir_exists(workspace.path());
    let install_root = workspace.path().join("installed-skill");

    let output = run_installer(
        repo_root(),
        &[install_root.clone().into_os_string()],
        &[("CARGO_NET_OFFLINE", OsString::from("true"))],
        &[],
    )?;
    assert_installer_success(&output, "install-gen-plan-skill.sh positional path")?;

    assert_eq!(
        installed_root_from_output(&output)?,
        install_root,
        "installer should print the final install root path"
    );
    assert_installed_bundle(&install_root)?;
    Ok(())
}

#[test]
fn install_script_resolves_relative_path_argument_from_caller_cwd() -> Result<()> {
    let workspace = support::workspace()?;
    let caller_cwd = workspace.path().join("caller");
    fs::create_dir_all(&caller_cwd)?;
    let relative_path = format!(
        "relative-install-{}/loopy-gen-plan",
        workspace
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("workspace fixture name should be utf-8")?
    );
    let install_root = caller_cwd.join(&relative_path);

    let output = run_installer(
        &caller_cwd,
        &[
            OsString::from("--path"),
            OsString::from(&relative_path),
        ],
        &[("CARGO_NET_OFFLINE", OsString::from("true"))],
        &[],
    )?;
    assert_installer_success(&output, "install-gen-plan-skill.sh --path relative")?;

    assert_eq!(
        installed_root_from_output(&output)?,
        install_root,
        "relative custom paths should resolve against the caller cwd"
    );
    assert_installed_bundle(&install_root)?;
    assert!(
        !repo_root().join(&relative_path).exists(),
        "relative install roots must not be resolved from the repo root"
    );
    Ok(())
}

#[test]
fn install_script_supports_target_codex() -> Result<()> {
    let workspace = support::workspace()?;
    let codex_home = workspace.path().join("codex-home");
    let install_root = codex_home.join("skills/loopy-gen-plan");

    let output = run_installer(
        repo_root(),
        &[OsString::from("--target"), OsString::from("codex")],
        &[
            ("CARGO_NET_OFFLINE", OsString::from("true")),
            ("CODEX_HOME", codex_home.into_os_string()),
        ],
        &[],
    )?;
    assert_installer_success(&output, "install-gen-plan-skill.sh --target codex")?;

    assert_eq!(installed_root_from_output(&output)?, install_root);
    assert_installed_bundle(&install_root)?;
    Ok(())
}

#[test]
fn install_script_resolves_relative_codex_home_from_caller_cwd() -> Result<()> {
    let workspace = support::workspace()?;
    let caller_cwd = workspace.path().join("caller-codex");
    fs::create_dir_all(&caller_cwd)?;
    let relative_home = format!(
        "rel-codex-{}/codex-home",
        workspace
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("workspace fixture name should be utf-8")?
    );
    let install_root = caller_cwd.join(&relative_home).join("skills/loopy-gen-plan");

    let output = run_installer(
        &caller_cwd,
        &[OsString::from("--target"), OsString::from("codex")],
        &[
            ("CARGO_NET_OFFLINE", OsString::from("true")),
            ("CODEX_HOME", OsString::from(&relative_home)),
        ],
        &[],
    )?;
    assert_installer_success(&output, "install-gen-plan-skill.sh --target codex with relative CODEX_HOME")?;

    assert_eq!(installed_root_from_output(&output)?, install_root);
    assert_installed_bundle(&install_root)?;
    assert!(
        !repo_root().join(&relative_home).exists(),
        "relative CODEX_HOME must not be resolved from the repo root"
    );
    Ok(())
}

#[test]
fn install_script_supports_target_claude() -> Result<()> {
    let workspace = support::workspace()?;
    let home = workspace.path().join("home");
    let install_root = home.join(".claude/skills/loopy-gen-plan");

    let output = run_installer(
        repo_root(),
        &[OsString::from("--target"), OsString::from("claude")],
        &[
            ("CARGO_NET_OFFLINE", OsString::from("true")),
            ("HOME", home.into_os_string()),
        ],
        &["CODEX_HOME"],
    )?;
    assert_installer_success(&output, "install-gen-plan-skill.sh --target claude")?;

    assert_eq!(installed_root_from_output(&output)?, install_root);
    assert_installed_bundle(&install_root)?;
    Ok(())
}

#[test]
fn install_script_resolves_relative_home_for_claude_target_from_caller_cwd() -> Result<()> {
    let workspace = support::workspace()?;
    let caller_cwd = workspace.path().join("caller-claude");
    fs::create_dir_all(&caller_cwd)?;
    let relative_home = format!(
        "rel-home-{}/home",
        workspace
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("workspace fixture name should be utf-8")?
    );
    let install_root = caller_cwd
        .join(&relative_home)
        .join(".claude/skills/loopy-gen-plan");

    let output = run_installer(
        &caller_cwd,
        &[OsString::from("--target"), OsString::from("claude")],
        &[
            ("CARGO_NET_OFFLINE", OsString::from("true")),
            ("HOME", OsString::from(&relative_home)),
        ],
        &["CODEX_HOME"],
    )?;
    assert_installer_success(&output, "install-gen-plan-skill.sh --target claude with relative HOME")?;

    assert_eq!(installed_root_from_output(&output)?, install_root);
    assert_installed_bundle(&install_root)?;
    assert!(
        !repo_root().join(&relative_home).exists(),
        "relative HOME must not be resolved from the repo root"
    );
    Ok(())
}

#[test]
fn install_script_rejects_unknown_flags_cleanly() -> Result<()> {
    let workspace = support::workspace()?;
    let output = run_installer(
        workspace.path(),
        &[OsString::from("--bogus")],
        &[],
        &[],
    )?;

    assert!(
        !output.status.success(),
        "expected unknown flags to fail, stdout was:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("unexpected flag: --bogus"),
        "expected clean unknown-flag error, stderr was:\n{stderr}"
    );

    Ok(())
}

fn run_installer(
    current_dir: &Path,
    args: &[OsString],
    envs: &[(&str, OsString)],
    removed_envs: &[&str],
) -> Result<Output> {
    let script_path = repo_root().join("scripts/install-gen-plan-skill.sh");
    let mut command = Command::new("bash");
    command.arg(script_path);
    for arg in args {
        command.arg(arg);
    }
    command.current_dir(current_dir);
    for (key, value) in envs {
        command.env(key, value);
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    command
        .output()
        .with_context(|| format!("failed to run installer from {}", current_dir.display()))
}

fn assert_installer_success(output: &Output, context: &str) -> Result<()> {
    if !output.status.success() {
        bail!(
            "{context} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn installed_root_from_output(output: &Output) -> Result<PathBuf> {
    let stdout = String::from_utf8(output.stdout.clone()).context("installer stdout was not utf-8")?;
    let line = stdout
        .lines()
        .last()
        .context("installer output should include the final install root")?;
    Ok(PathBuf::from(line))
}

fn assert_installed_bundle(install_root: &Path) -> Result<()> {
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

    let installed_skill = fs::read_to_string(install_root.join("SKILL.md"))?;
    assert!(
        installed_skill.contains(
            "$ loopy:gen-plan --input draft.md --plan-name rust-cli-todo --task-type coding-task"
        ),
        "installed SKILL.md should contain the new plan-name/task-type invocation"
    );
    assert!(
        !installed_skill.contains("--output"),
        "installed SKILL.md should no longer mention --output"
    );
    for required_snippet in [
        "Every candidate leaf must pass `leaf review gate`",
        "Every frontier parent expansion must pass `frontier review gate`",
        "send the review-driven revision back to the user",
        "If review-driven changes altered the structure, the Agent MUST ask the user to re-confirm the revised expansion before writing it or continuing.",
        "pause only for true user-owned decisions that cannot be inferred safely",
    ] {
        assert!(
            installed_skill.contains(required_snippet),
            "installed SKILL.md should contain `{required_snippet}`"
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
