use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

// Owns fixed bundle descriptor parsing and workspace-local development registry loading.

pub const DESCRIPTOR_FILE_NAME: &str = "bundle.toml";
pub const DEV_REGISTRY_RELATIVE_PATH: &str = "skills/dev-registry.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleDescriptor {
    pub skill_id: String,
    pub skill_kind: String,
    pub version: String,
    pub loader_id: String,
    pub root_entry: String,
    pub binary_path: String,
    pub internal_manifest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentRegistry {
    pub skills: Vec<DevelopmentSkillRegistration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentSkillRegistration {
    pub skill_id: String,
    pub loader_id: String,
    pub source_root: String,
    pub binary_package: String,
    pub binary_name: String,
    pub internal_manifest: String,
}

pub fn read_descriptor(bundle_root: &Path) -> Result<BundleDescriptor> {
    let descriptor_path = descriptor_path(bundle_root);
    let descriptor_text = std::fs::read_to_string(&descriptor_path)
        .with_context(|| format!("failed to read {}", descriptor_path.display()))?;
    toml::from_str(&descriptor_text)
        .with_context(|| format!("failed to parse {}", descriptor_path.display()))
}

pub fn read_development_registry(workspace_root: &Path) -> Result<DevelopmentRegistry> {
    let registry_path = workspace_root.join(DEV_REGISTRY_RELATIVE_PATH);
    let registry_text = std::fs::read_to_string(&registry_path)
        .with_context(|| format!("failed to read {}", registry_path.display()))?;
    toml::from_str(&registry_text)
        .with_context(|| format!("failed to parse {}", registry_path.display()))
}

pub fn find_registered_skill<'a>(
    registry: &'a DevelopmentRegistry,
    skill_id: &str,
) -> Result<&'a DevelopmentSkillRegistration> {
    registry
        .skills
        .iter()
        .find(|registration| registration.skill_id == skill_id)
        .ok_or_else(|| anyhow!("development registry is missing {skill_id}"))
}

pub fn resolve_registered_source_root(
    workspace_root: &Path,
    registration: &DevelopmentSkillRegistration,
) -> PathBuf {
    workspace_root.join(&registration.source_root)
}

pub fn discover_installed_skill(
    skill_id: &str,
    candidate_roots: &[PathBuf],
) -> Result<(PathBuf, BundleDescriptor)> {
    for root in candidate_roots {
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(root)
            .with_context(|| format!("failed to read {}", root.display()))?
        {
            let entry = entry?;
            let bundle_root = entry.path();
            let descriptor_path = descriptor_path(&bundle_root);
            if !descriptor_path.is_file() {
                continue;
            }
            let descriptor = read_descriptor(&bundle_root)?;
            if descriptor.skill_id == skill_id {
                return Ok((bundle_root, descriptor));
            }
        }
    }
    bail!("failed to discover installed skill {skill_id}")
}

pub fn descriptor_path(bundle_root: &Path) -> PathBuf {
    bundle_root.join(DESCRIPTOR_FILE_NAME)
}
