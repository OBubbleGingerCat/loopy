// System helpers own process, git, filesystem, and clock interactions; business rules stay elsewhere.

use super::*;

pub(crate) fn read_json_file(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

pub(crate) fn write_invocation_context_file(invocation_context_payload: &Value) -> Result<()> {
    let invocation_context_path = invocation_context_payload
        .get("invocation_context_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("invocation context payload missing invocation_context_path"))?;
    let invocation_context_path = PathBuf::from(invocation_context_path);
    if let Some(parent) = invocation_context_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        &invocation_context_path,
        serde_json::to_string_pretty(invocation_context_payload)?,
    )
    .with_context(|| format!("failed to write {}", invocation_context_path.display()))?;
    Ok(())
}

pub(crate) fn git_verify(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn run_local_command(
    command: &[String],
    cwd: &Path,
    stdin_payload: Option<&str>,
    timeout_sec: i64,
    env_policy: &str,
    env_allow: &[String],
) -> Result<std::process::Output> {
    loopy_common_invocation::run_local_command(
        command,
        cwd,
        stdin_payload,
        timeout_sec,
        env_policy,
        env_allow,
    )
}

pub(crate) fn git_head_sha(workspace_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("failed to read git HEAD from {}", workspace_root.display()))?;
    if !output.status.success() {
        bail!(
            "failed to resolve git HEAD: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let sha = String::from_utf8(output.stdout)?.trim().to_owned();
    if sha.is_empty() {
        bail!("git HEAD was empty");
    }
    Ok(sha)
}

pub(crate) fn git_current_branch(workspace_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .with_context(|| {
            format!(
                "failed to read current git branch from {}",
                workspace_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "failed to resolve current git branch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let branch = String::from_utf8(output.stdout)?.trim().to_owned();
    if branch.is_empty() || branch == "HEAD" {
        bail!("current git branch is detached");
    }
    Ok(branch)
}

pub(crate) fn timestamp() -> Result<String> {
    loopy_common_events::timestamp()
}
