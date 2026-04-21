use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use loopy_common_bundle::{read_descriptor, BundleDescriptor};
use serde::{Deserialize, Serialize};

pub const SKILL_ID: &str = "loopy:gen-plan";
pub const LOADER_ID: &str = "loopy.gen-plan.v1";

const DOMAIN_CONTRACT_PROMPT: &str = "domain_contract";
const LEAF_RUNTIME_PROMPT: &str = "leaf_runtime";
const FRONTIER_RUNTIME_PROMPT: &str = "frontier_runtime";
const LEAF_REVIEWER_ROLE_KIND: &str = "leaf_reviewer";
const FRONTIER_REVIEWER_ROLE_KIND: &str = "frontier_reviewer";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub skill: SkillConfig,
    pub executors: HashMap<String, ExecutorProfile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillConfig {
    pub name: String,
    pub default_install_target: String,
    #[serde(default)]
    pub install_targets: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorProfile {
    pub kind: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub timeout_sec: i64,
    pub transcript_capture: String,
    #[serde(default)]
    pub env_allow: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTypeConfig {
    pub task_type: String,
    pub default_leaf_reviewer: String,
    pub default_frontier_reviewer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedGateRoleSelection {
    pub task_type: String,
    pub leaf_reviewer_role_id: String,
    pub frontier_reviewer_role_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleFrontMatter {
    pub role: String,
    pub executor: String,
}

pub fn validate_placeholder_bundle(bundle_root: &Path) -> Result<()> {
    load_bundle_descriptor(bundle_root).map(|_| ())
}

pub fn load_bundle_descriptor(skill_root: &Path) -> Result<BundleDescriptor> {
    let mut descriptor = read_descriptor(skill_root)?;
    if descriptor.skill_id != SKILL_ID {
        bail!(
            "expected skill_id {} in {}, found {}",
            SKILL_ID,
            skill_root.join("bundle.toml").display(),
            descriptor.skill_id
        );
    }
    if descriptor.loader_id != LOADER_ID {
        bail!(
            "expected loader_id {} in {}, found {}",
            LOADER_ID,
            skill_root.join("bundle.toml").display(),
            descriptor.loader_id
        );
    }
    descriptor.internal_manifest =
        validate_bundle_local_file_name("internal_manifest", &descriptor.internal_manifest)?;
    Ok(descriptor)
}

pub fn load_manifest(skill_root: &Path) -> Result<Manifest> {
    let descriptor = load_bundle_descriptor(skill_root)?;
    let manifest_path = skill_root.join(descriptor.internal_manifest);
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest_text)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    if manifest.skill.name != SKILL_ID {
        bail!(
            "expected manifest skill.name {} in {}, found {}",
            SKILL_ID,
            manifest_path.display(),
            manifest.skill.name
        );
    }
    if !manifest
        .skill
        .install_targets
        .contains_key(&manifest.skill.default_install_target)
    {
        bail!(
            "default_install_target {} is not defined in {}",
            manifest.skill.default_install_target,
            manifest_path.display()
        );
    }
    Ok(manifest)
}

pub fn load_task_type_config(skill_root: &Path, task_type: &str) -> Result<TaskTypeConfig> {
    let task_type = validate_task_type_identifier(task_type)?;
    let config_path = skill_root
        .join("roles")
        .join(&task_type)
        .join("task-type.toml");
    if !config_path.is_file() {
        bail!(
            "unknown task_type {task_type}: missing {}",
            config_path.display()
        );
    }
    let config_text = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let config: TaskTypeConfig = toml::from_str(&config_text)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    let config_task_type = validate_task_type_identifier(&config.task_type)
        .with_context(|| format!("invalid task_type in {}", config_path.display()))?;
    if config_task_type != task_type {
        bail!(
            "task_type {} does not match {} in {}",
            task_type,
            config_task_type,
            config_path.display()
        );
    }
    Ok(config)
}

pub fn resolve_gate_roles(
    skill_root: &Path,
    manifest: &Manifest,
    task_type: &str,
) -> Result<ResolvedGateRoleSelection> {
    let task_type = validate_task_type_identifier(task_type)?;
    let task_type_config = load_task_type_config(skill_root, &task_type)?;
    let leaf_reviewer_role_id = normalize_role_id(
        "default_leaf_reviewer",
        &task_type_config.default_leaf_reviewer,
    )?;
    let frontier_reviewer_role_id = normalize_role_id(
        "default_frontier_reviewer",
        &task_type_config.default_frontier_reviewer,
    )?;

    validate_selected_role(
        skill_root,
        manifest,
        &task_type,
        "leaf_reviewer",
        &leaf_reviewer_role_id,
    )?;
    validate_selected_role(
        skill_root,
        manifest,
        &task_type,
        "frontier_reviewer",
        &frontier_reviewer_role_id,
    )?;

    Ok(ResolvedGateRoleSelection {
        task_type,
        leaf_reviewer_role_id,
        frontier_reviewer_role_id,
    })
}

pub fn load_task_type_role_definition(
    skill_root: &Path,
    manifest: &Manifest,
    task_type: &str,
    role_kind: &str,
    role_id: &str,
) -> Result<(PathBuf, String, RoleFrontMatter, ExecutorProfile)> {
    let role_path = role_path_for(skill_root, task_type, role_kind, role_id)?;
    let role_markdown = fs::read_to_string(&role_path)
        .with_context(|| format!("failed to read {}", role_path.display()))?;
    let role_front_matter = parse_role_front_matter(&role_markdown)
        .with_context(|| format!("failed to parse front matter in {}", role_path.display()))?;
    let role_prompt_markdown = extract_role_body(&role_markdown)?;
    if role_front_matter.role != role_kind {
        bail!(
            "role kind mismatch for {}: expected {}, found {}",
            role_path.display(),
            role_kind,
            role_front_matter.role
        );
    }
    let executor_profile = manifest
        .executors
        .get(&role_front_matter.executor)
        .cloned()
        .ok_or_else(|| anyhow!("missing executor profile {}", role_front_matter.executor))?;
    Ok((
        role_path,
        role_prompt_markdown,
        role_front_matter,
        executor_profile,
    ))
}

pub fn load_domain_contract_prompt(skill_root: &Path) -> Result<String> {
    load_prompt_template(skill_root, DOMAIN_CONTRACT_PROMPT)
}

pub fn load_leaf_runtime_prompt(skill_root: &Path) -> Result<String> {
    load_prompt_template(skill_root, LEAF_RUNTIME_PROMPT)
}

pub fn load_frontier_runtime_prompt(skill_root: &Path) -> Result<String> {
    load_prompt_template(skill_root, FRONTIER_RUNTIME_PROMPT)
}

pub fn load_prompt_template(skill_root: &Path, template_name: &str) -> Result<String> {
    let template_name = validate_template_name(template_name)?;
    let prompt_path = skill_root
        .join("prompts")
        .join(format!("{template_name}.md"));
    let prompt_markdown = fs::read_to_string(&prompt_path)
        .with_context(|| format!("failed to read {}", prompt_path.display()))?;
    Ok(prompt_markdown.trim().to_owned())
}

pub fn resolve_executor_command(
    executor_profile: &ExecutorProfile,
    bundle_bin: &Path,
    workspace_root: &Path,
    project_directory: &Path,
    invocation_payload_path: &Path,
    output_last_message_path: &Path,
) -> Vec<String> {
    let mut command = Vec::with_capacity(1 + executor_profile.args.len());
    command.push(resolve_template_value(
        &executor_profile.command,
        bundle_bin,
        workspace_root,
        project_directory,
        invocation_payload_path,
        output_last_message_path,
    ));
    command.extend(executor_profile.args.iter().map(|arg| {
        resolve_template_value(
            arg,
            bundle_bin,
            workspace_root,
            project_directory,
            invocation_payload_path,
            output_last_message_path,
        )
    }));
    command
}

pub fn resolve_executor_cwd(cwd: &str, workspace_root: &Path, project_directory: &Path) -> String {
    match cwd {
        "project" => project_directory.display().to_string(),
        "workspace" => workspace_root.display().to_string(),
        other => other.to_owned(),
    }
}

fn resolve_template_value(
    value: &str,
    bundle_bin: &Path,
    workspace_root: &Path,
    project_directory: &Path,
    invocation_payload_path: &Path,
    output_last_message_path: &Path,
) -> String {
    value
        .replace("{bundle_bin}", &bundle_bin.display().to_string())
        .replace("{workspace_root}", &workspace_root.display().to_string())
        .replace(
            "{project_directory}",
            &project_directory.display().to_string(),
        )
        .replace(
            "{invocation_payload_path}",
            &invocation_payload_path.display().to_string(),
        )
        .replace(
            "{output_last_message_path}",
            &output_last_message_path.display().to_string(),
        )
}

fn normalize_non_blank(field_name: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field_name} must not be blank");
    }
    Ok(trimmed.to_owned())
}

fn normalize_role_id(field_name: &str, value: &str) -> Result<String> {
    validate_ascii_identifier_with_separators(field_name, value, &['_', '-'])
}

pub fn validate_task_type_identifier(task_type: &str) -> Result<String> {
    validate_ascii_identifier_with_separators("task_type", task_type, &['-'])
}

fn validate_role_kind(role_kind: &str) -> Result<&'static str> {
    match role_kind {
        LEAF_REVIEWER_ROLE_KIND => Ok(LEAF_REVIEWER_ROLE_KIND),
        FRONTIER_REVIEWER_ROLE_KIND => Ok(FRONTIER_REVIEWER_ROLE_KIND),
        _ => bail!(
            "role_kind must be one of `{LEAF_REVIEWER_ROLE_KIND}` or `{FRONTIER_REVIEWER_ROLE_KIND}`"
        ),
    }
}

fn validate_template_name(template_name: &str) -> Result<String> {
    validate_ascii_identifier_with_separators("template_name", template_name, &['_'])
}

fn validate_bundle_local_file_name(field_name: &str, value: &str) -> Result<String> {
    let normalized = normalize_non_blank(field_name, value)?;
    if normalized.contains('/') || normalized.contains('\\') {
        bail!("{field_name} must be a bundle-local file name without path separators");
    }

    let path = Path::new(&normalized);
    if path.is_absolute() {
        bail!("{field_name} must be a bundle-local file name");
    }

    let mut components = path.components();
    let Some(std::path::Component::Normal(component)) = components.next() else {
        bail!("{field_name} must be a bundle-local file name");
    };
    if components.next().is_some() || component != normalized.as_str() {
        bail!("{field_name} must be a bundle-local file name");
    }

    Ok(normalized)
}

fn validate_ascii_identifier_with_separators(
    field_name: &str,
    value: &str,
    allowed_separators: &[char],
) -> Result<String> {
    let normalized = normalize_non_blank(field_name, value)?;
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        bail!("{field_name} must not be blank");
    };
    if !first.is_ascii_lowercase() {
        bail!(
            "{field_name} must be a safe identifier using lowercase ascii letters, digits, and internal separators"
        );
    }

    let mut previous_was_separator = false;
    for ch in normalized.chars() {
        match ch {
            'a'..='z' | '0'..='9' => previous_was_separator = false,
            separator if allowed_separators.contains(&separator) => {
                if previous_was_separator {
                    bail!(
                        "{field_name} must be a safe identifier using lowercase ascii letters, digits, and internal separators"
                    );
                }
                previous_was_separator = true;
            }
            _ => {
                bail!(
                    "{field_name} must be a safe identifier using lowercase ascii letters, digits, and internal separators"
                );
            }
        }
    }

    if normalized
        .chars()
        .last()
        .is_some_and(|ch| allowed_separators.contains(&ch))
    {
        bail!(
            "{field_name} must be a safe identifier using lowercase ascii letters, digits, and internal separators"
        );
    }

    Ok(normalized)
}

fn validate_selected_role(
    skill_root: &Path,
    manifest: &Manifest,
    task_type: &str,
    role_kind: &str,
    role_id: &str,
) -> Result<()> {
    let role_path = role_path_for(skill_root, task_type, role_kind, role_id)?;
    if !role_path.is_file() {
        bail!(
            "missing role file for {} {} at {}",
            role_kind,
            role_id,
            role_path.display()
        );
    }
    let role_markdown = fs::read_to_string(&role_path)
        .with_context(|| format!("failed to read {}", role_path.display()))?;
    let front_matter = parse_role_front_matter(&role_markdown)
        .with_context(|| format!("failed to parse front matter in {}", role_path.display()))?;
    if front_matter.role != role_kind {
        bail!(
            "role kind mismatch for {}: expected {}, found {}",
            role_path.display(),
            role_kind,
            front_matter.role
        );
    }
    if !manifest.executors.contains_key(&front_matter.executor) {
        bail!(
            "unknown executor {} declared by {}",
            front_matter.executor,
            role_path.display()
        );
    }
    Ok(())
}

fn role_path_for(
    skill_root: &Path,
    task_type: &str,
    role_kind: &str,
    role_id: &str,
) -> Result<PathBuf> {
    let role_kind = validate_role_kind(role_kind)?;
    let role_id = normalize_role_id("role_id", role_id)?;
    Ok(skill_root
        .join("roles")
        .join(validate_task_type_identifier(task_type)?)
        .join(role_kind)
        .join(format!("{role_id}.md")))
}

fn parse_role_front_matter(markdown: &str) -> Result<RoleFrontMatter> {
    let (front_matter, _) = split_role_markdown(markdown)?;
    Ok(toml::from_str(front_matter).context("failed to parse role front matter")?)
}

fn extract_role_body(markdown: &str) -> Result<String> {
    let (_, body) = split_role_markdown(markdown)?;
    Ok(body.trim().to_owned())
}

fn split_role_markdown(markdown: &str) -> Result<(&str, &str)> {
    let mut sections = markdown.splitn(3, "---");
    let prefix = sections.next().unwrap_or_default();
    let front_matter = sections
        .next()
        .ok_or_else(|| anyhow!("missing role front matter"))?;
    let body = sections
        .next()
        .ok_or_else(|| anyhow!("missing role body after front matter"))?;
    if !prefix.trim().is_empty() {
        bail!("unexpected content before role front matter");
    }
    Ok((front_matter, body))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;

    use super::*;

    #[test]
    fn invalid_task_type_is_rejected_before_role_path_resolution() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;

        let manifest = load_manifest(skill_root.path())?;
        let error = resolve_gate_roles(skill_root.path(), &manifest, "../coding-task")
            .expect_err("invalid task_type should be rejected");
        assert!(format!("{error:#}").contains("task_type"));

        Ok(())
    }

    #[test]
    fn malformed_role_front_matter_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;
        write_task_type_config(skill_root.path(), "coding-task", "broken", "codex_default")?;
        write_role(
            skill_root.path(),
            "coding-task",
            "leaf_reviewer",
            "broken",
            "---\nrole = \"leaf_reviewer\"\nexecutor = [\n---\nbody\n",
        )?;

        let manifest = load_manifest(skill_root.path())?;
        let error = load_task_type_role_definition(
            skill_root.path(),
            &manifest,
            "coding-task",
            "leaf_reviewer",
            "broken",
        )
        .expect_err("malformed front matter should fail");
        assert!(format!("{error:#}").contains("front matter"));

        Ok(())
    }

    #[test]
    fn missing_role_file_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;
        write_task_type_config(
            skill_root.path(),
            "coding-task",
            "missing_leaf",
            "codex_default",
        )?;
        write_role(
            skill_root.path(),
            "coding-task",
            "frontier_reviewer",
            "codex_default",
            valid_role_markdown("frontier_reviewer", "codex_frontier_reviewer"),
        )?;

        let manifest = load_manifest(skill_root.path())?;
        let error = resolve_gate_roles(skill_root.path(), &manifest, "coding-task")
            .expect_err("missing role file should fail");
        assert!(format!("{error:#}").contains("missing role file"));

        Ok(())
    }

    #[test]
    fn unknown_executor_profile_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;
        write_task_type_config(
            skill_root.path(),
            "coding-task",
            "codex_default",
            "codex_default",
        )?;
        write_role(
            skill_root.path(),
            "coding-task",
            "leaf_reviewer",
            "codex_default",
            valid_role_markdown("leaf_reviewer", "missing_executor"),
        )?;
        write_role(
            skill_root.path(),
            "coding-task",
            "frontier_reviewer",
            "codex_default",
            valid_role_markdown("frontier_reviewer", "codex_frontier_reviewer"),
        )?;

        let manifest = load_manifest(skill_root.path())?;
        let error = resolve_gate_roles(skill_root.path(), &manifest, "coding-task")
            .expect_err("unknown executor profile should fail");
        assert!(format!("{error:#}").contains("unknown executor"));

        Ok(())
    }

    #[test]
    fn missing_prompt_template_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;

        let error = load_prompt_template(skill_root.path(), "missing_prompt")
            .expect_err("missing prompt should fail");
        assert!(format!("{error:#}").contains("failed to read"));

        Ok(())
    }

    #[test]
    fn role_id_with_path_separator_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;
        write_task_type_config(
            skill_root.path(),
            "coding-task",
            "codex_default",
            "codex_default",
        )?;

        let manifest = load_manifest(skill_root.path())?;
        let error = load_task_type_role_definition(
            skill_root.path(),
            &manifest,
            "coding-task",
            "leaf_reviewer",
            "../codex_default",
        )
        .expect_err("path-bearing role_id should fail");
        assert!(format!("{error:#}").contains("role_id"));

        Ok(())
    }

    #[test]
    fn invalid_role_kind_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;
        write_task_type_config(
            skill_root.path(),
            "coding-task",
            "codex_default",
            "codex_default",
        )?;

        let manifest = load_manifest(skill_root.path())?;
        let error = load_task_type_role_definition(
            skill_root.path(),
            &manifest,
            "coding-task",
            "checkpoint_reviewer",
            "codex_default",
        )
        .expect_err("unexpected role_kind should fail");
        assert!(format!("{error:#}").contains("role_kind"));

        Ok(())
    }

    #[test]
    fn template_name_with_path_separator_is_rejected() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        write_valid_bundle(skill_root.path())?;

        let error = load_prompt_template(skill_root.path(), "../leaf_runtime")
            .expect_err("path-bearing template_name should fail");
        assert!(format!("{error:#}").contains("template_name"));

        Ok(())
    }

    #[test]
    fn bundle_descriptor_rejects_wrong_skill_id() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        fs::create_dir_all(skill_root.path())?;
        fs::write(
            skill_root.path().join("bundle.toml"),
            [
                "skill_id = \"loopy:not-gen-plan\"",
                "skill_kind = \"plan_generation\"",
                "version = \"0.1.0\"",
                "loader_id = \"loopy.gen-plan.v1\"",
                "root_entry = \"SKILL.md\"",
                "binary_path = \"bin/loopy-gen-plan\"",
                "internal_manifest = \"gen-plan.toml\"",
                "",
            ]
            .join("\n"),
        )?;

        let error =
            load_bundle_descriptor(skill_root.path()).expect_err("wrong skill_id should fail");
        assert!(format!("{error:#}").contains("expected skill_id"));

        Ok(())
    }

    #[test]
    fn bundle_descriptor_rejects_wrong_loader_id() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        fs::create_dir_all(skill_root.path())?;
        fs::write(
            skill_root.path().join("bundle.toml"),
            [
                "skill_id = \"loopy:gen-plan\"",
                "skill_kind = \"plan_generation\"",
                "version = \"0.1.0\"",
                "loader_id = \"loopy.not-gen-plan.v1\"",
                "root_entry = \"SKILL.md\"",
                "binary_path = \"bin/loopy-gen-plan\"",
                "internal_manifest = \"gen-plan.toml\"",
                "",
            ]
            .join("\n"),
        )?;

        let error =
            load_bundle_descriptor(skill_root.path()).expect_err("wrong loader_id should fail");
        assert!(format!("{error:#}").contains("expected loader_id"));

        Ok(())
    }

    #[test]
    fn manifest_loading_rejects_path_bearing_internal_manifest() -> Result<()> {
        let skill_root = tempfile::tempdir()?;
        fs::create_dir_all(skill_root.path())?;
        fs::write(skill_root.path().join("SKILL.md"), "# gen-plan\n")?;
        fs::write(
            skill_root.path().join("bundle.toml"),
            [
                "skill_id = \"loopy:gen-plan\"",
                "skill_kind = \"plan_generation\"",
                "version = \"0.1.0\"",
                "loader_id = \"loopy.gen-plan.v1\"",
                "root_entry = \"SKILL.md\"",
                "binary_path = \"bin/loopy-gen-plan\"",
                "internal_manifest = \"../gen-plan.toml\"",
                "",
            ]
            .join("\n"),
        )?;

        let error = load_manifest(skill_root.path())
            .expect_err("path-bearing internal_manifest should fail");
        assert!(format!("{error:#}").contains("internal_manifest"));

        Ok(())
    }

    #[test]
    fn checked_in_gen_plan_bundle_assets_load_and_preserve_project_executor_contract() -> Result<()>
    {
        let skill_root = checked_in_gen_plan_skill_root()?;

        let manifest = load_manifest(&skill_root)?;
        let domain_contract = load_domain_contract_prompt(&skill_root)?;
        let leaf_runtime = load_leaf_runtime_prompt(&skill_root)?;
        let frontier_runtime = load_frontier_runtime_prompt(&skill_root)?;

        assert!(domain_contract.contains("Gen-Plan Domain Contract"));
        assert!(leaf_runtime.contains("Leaf Node Review Gate"));
        assert!(frontier_runtime.contains("Frontier Review Gate"));

        let (_, leaf_prompt, leaf_front_matter, leaf_executor) = load_task_type_role_definition(
            &skill_root,
            &manifest,
            "coding-task",
            "leaf_reviewer",
            "codex_default",
        )?;
        let (_, frontier_prompt, frontier_front_matter, frontier_executor) =
            load_task_type_role_definition(
                &skill_root,
                &manifest,
                "coding-task",
                "frontier_reviewer",
                "codex_default",
            )?;

        assert_eq!(leaf_front_matter.executor, "codex_leaf_reviewer");
        assert_eq!(frontier_front_matter.executor, "codex_frontier_reviewer");
        assert!(!leaf_prompt.trim().is_empty());
        assert!(!frontier_prompt.trim().is_empty());

        let mock_leaf_executor = manifest
            .executors
            .get("mock_leaf_reviewer")
            .expect("checked-in manifest should define mock leaf reviewer");
        let mock_frontier_executor = manifest
            .executors
            .get("mock_frontier_reviewer")
            .expect("checked-in manifest should define mock frontier reviewer");

        for executor in [&leaf_executor, &frontier_executor] {
            assert_eq!(executor.cwd, "project");
            assert!(
                executor.args.windows(2).any(|window| {
                    window[0] == "-o" && window[1] == "{output_last_message_path}"
                }),
                "executor args should capture the stable last-message path: {:?}",
                executor.args
            );
            assert!(
                executor
                    .args
                    .iter()
                    .any(|arg| arg == "--skip-git-repo-check"),
                "executor args should skip git repo trust checks for reviewer runs: {:?}",
                executor.args
            );
            assert!(
                !executor.args.iter().any(|arg| arg.contains("worktree")),
                "executor args should not assume worktrees: {:?}",
                executor.args
            );
            assert!(
                !executor
                    .args
                    .iter()
                    .any(|arg| arg.contains("dangerously-bypass-approvals-and-sandbox")),
                "executor args should not enable bypass-sandbox mode: {:?}",
                executor.args
            );
        }
        for executor in [mock_leaf_executor, mock_frontier_executor] {
            assert_eq!(executor.cwd, "project");
            assert!(
                executor.args.windows(2).any(|window| {
                    window[0] == "--output-last-message"
                        && window[1] == "{output_last_message_path}"
                }),
                "mock executor args should capture the stable last-message path: {:?}",
                executor.args
            );
        }
        assert!(frontier_runtime.contains("approved_frontier"));

        Ok(())
    }

    #[test]
    fn resolve_executor_command_replaces_project_and_last_message_placeholders() -> Result<()> {
        let executor_profile = ExecutorProfile {
            kind: "local_command".to_owned(),
            command: "codex".to_owned(),
            args: vec![
                "exec".to_owned(),
                "-C".to_owned(),
                "{project_directory}".to_owned(),
                "-o".to_owned(),
                "{output_last_message_path}".to_owned(),
                "{invocation_payload_path}".to_owned(),
            ],
            cwd: "project".to_owned(),
            timeout_sec: 60,
            transcript_capture: "stdio".to_owned(),
            env_allow: None,
        };

        let command = resolve_executor_command(
            &executor_profile,
            Path::new("/tmp/skill/bin/loopy-gen-plan"),
            Path::new("/tmp/workspace"),
            Path::new("/tmp/workspace/project"),
            Path::new("/tmp/workspace/.loopy/gates/gate-1/prompt.md"),
            Path::new("/tmp/workspace/.loopy/gates/gate-1/last-message.json"),
        );

        assert_eq!(command[0], "codex");
        assert_eq!(command[1], "exec");
        assert_eq!(command[3], "/tmp/workspace/project");
        assert_eq!(
            command[5],
            "/tmp/workspace/.loopy/gates/gate-1/last-message.json"
        );
        assert_eq!(command[6], "/tmp/workspace/.loopy/gates/gate-1/prompt.md");

        Ok(())
    }

    #[test]
    fn resolve_executor_cwd_maps_project_to_project_directory() {
        assert_eq!(
            resolve_executor_cwd(
                "project",
                Path::new("/tmp/workspace"),
                Path::new("/tmp/workspace/project"),
            ),
            "/tmp/workspace/project"
        );
        assert_eq!(
            resolve_executor_cwd(
                "workspace",
                Path::new("/tmp/workspace"),
                Path::new("/tmp/workspace/project"),
            ),
            "/tmp/workspace"
        );
    }

    fn write_valid_bundle(skill_root: &Path) -> Result<()> {
        fs::create_dir_all(skill_root.join("prompts"))?;
        fs::create_dir_all(skill_root.join("bin"))?;
        fs::write(skill_root.join("SKILL.md"), "# gen-plan\n")?;
        fs::write(skill_root.join("bin/loopy-gen-plan"), "#!/bin/sh\nexit 0\n")?;
        fs::write(
            skill_root.join("bundle.toml"),
            [
                "skill_id = \"loopy:gen-plan\"",
                "skill_kind = \"plan_generation\"",
                "version = \"0.1.0\"",
                "loader_id = \"loopy.gen-plan.v1\"",
                "root_entry = \"SKILL.md\"",
                "binary_path = \"bin/loopy-gen-plan\"",
                "internal_manifest = \"gen-plan.toml\"",
                "",
            ]
            .join("\n"),
        )?;
        fs::write(
            skill_root.join("gen-plan.toml"),
            [
                "[skill]",
                "name = \"loopy:gen-plan\"",
                "default_install_target = \"codex\"",
                "",
                "[skill.install_targets]",
                "codex = \"$HOME/.codex/skills/loopy-gen-plan\"",
                "",
                "[executors.codex_leaf_reviewer]",
                "kind = \"local_command\"",
                "command = \"codex\"",
                "args = [\"exec\"]",
                "cwd = \"project\"",
                "timeout_sec = 60",
                "transcript_capture = \"stdio\"",
                "",
                "[executors.codex_frontier_reviewer]",
                "kind = \"local_command\"",
                "command = \"codex\"",
                "args = [\"exec\"]",
                "cwd = \"project\"",
                "timeout_sec = 60",
                "transcript_capture = \"stdio\"",
                "",
            ]
            .join("\n"),
        )?;
        fs::write(skill_root.join("prompts/domain_contract.md"), "domain")?;
        fs::write(skill_root.join("prompts/leaf_runtime.md"), "leaf")?;
        fs::write(skill_root.join("prompts/frontier_runtime.md"), "frontier")?;
        Ok(())
    }

    fn write_task_type_config(
        skill_root: &Path,
        task_type: &str,
        leaf_reviewer: &str,
        frontier_reviewer: &str,
    ) -> Result<()> {
        let role_dir = skill_root.join("roles").join(task_type);
        fs::create_dir_all(role_dir.join("leaf_reviewer"))?;
        fs::create_dir_all(role_dir.join("frontier_reviewer"))?;
        fs::write(
            role_dir.join("task-type.toml"),
            format!(
                "task_type = \"{task_type}\"\ndefault_leaf_reviewer = \"{leaf_reviewer}\"\ndefault_frontier_reviewer = \"{frontier_reviewer}\"\n"
            ),
        )?;
        Ok(())
    }

    fn write_role(
        skill_root: &Path,
        task_type: &str,
        role_kind: &str,
        role_id: &str,
        markdown: impl AsRef<str>,
    ) -> Result<()> {
        let role_dir = skill_root.join("roles").join(task_type).join(role_kind);
        fs::create_dir_all(&role_dir)?;
        fs::write(role_dir.join(format!("{role_id}.md")), markdown.as_ref())?;
        Ok(())
    }

    fn valid_role_markdown(role_kind: &str, executor: &str) -> String {
        format!("---\nrole = \"{role_kind}\"\nexecutor = \"{executor}\"\n---\nbody\n")
    }

    fn checked_in_gen_plan_skill_root() -> Result<PathBuf> {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../skills/gen-plan")
            .canonicalize()
            .context("checked-in skills/gen-plan root should resolve")
    }
}
