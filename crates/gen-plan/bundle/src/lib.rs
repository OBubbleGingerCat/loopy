use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use loopy_common_bundle::{BundleDescriptor, read_descriptor};
use serde::{Deserialize, Serialize};

pub const SKILL_ID: &str = "loopy:gen-plan";
pub const LOADER_ID: &str = "loopy.gen-plan.v1";

const DOMAIN_CONTRACT_PROMPT: &str = "domain_contract";
const LEAF_RUNTIME_PROMPT: &str = "leaf_runtime";
const FRONTIER_RUNTIME_PROMPT: &str = "frontier_runtime";

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
    let descriptor = read_descriptor(skill_root)?;
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
    let config_path = skill_root
        .join("roles")
        .join(task_type)
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
    if config.task_type.trim() != task_type {
        bail!(
            "task_type {} does not match {} in {}",
            task_type,
            config.task_type,
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
    let task_type = normalize_non_blank("task_type", task_type)?;
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
    let role_path = role_path_for(skill_root, task_type, role_kind, role_id);
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
    let prompt_path = skill_root
        .join("prompts")
        .join(format!("{template_name}.md"));
    let prompt_markdown = fs::read_to_string(&prompt_path)
        .with_context(|| format!("failed to read {}", prompt_path.display()))?;
    Ok(prompt_markdown.trim().to_owned())
}

fn normalize_non_blank(field_name: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field_name} must not be blank");
    }
    Ok(trimmed.to_owned())
}

fn normalize_role_id(field_name: &str, value: &str) -> Result<String> {
    let normalized = normalize_non_blank(field_name, value)?;
    if normalized.contains('/') || normalized.contains('\\') {
        bail!("{field_name} must not contain path separators");
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
    let role_path = role_path_for(skill_root, task_type, role_kind, role_id);
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

fn role_path_for(skill_root: &Path, task_type: &str, role_kind: &str, role_id: &str) -> PathBuf {
    skill_root
        .join("roles")
        .join(task_type)
        .join(role_kind)
        .join(format!("{role_id}.md"))
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
