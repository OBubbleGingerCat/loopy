use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

// Owns fixed bundle descriptor parsing and workspace-local development registry loading.

pub const DESCRIPTOR_FILE_NAME: &str = "bundle.toml";
pub const DEV_REGISTRY_RELATIVE_PATH: &str = "skills/dev-registry.toml";

#[derive(Debug, Clone)]
pub struct DiscoveredBundle {
    pub bundle_root: PathBuf,
    pub descriptor: BundleDescriptor,
}

#[derive(Debug, Clone)]
pub struct ResolvedDevelopmentSkill {
    pub bundle_root: PathBuf,
    pub descriptor: BundleDescriptor,
    pub registration: DevelopmentSkillRegistration,
}

pub struct LoaderRegistration<T> {
    pub loader_id: &'static str,
    pub loader: T,
}

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
    let registry_path = development_registry_path(workspace_root);
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
    let source_root = PathBuf::from(&registration.source_root);
    if source_root.is_absolute() {
        source_root
    } else {
        workspace_root.join(source_root)
    }
}

pub fn resolve_development_skill(
    workspace_root: &Path,
    skill_id: &str,
) -> Result<ResolvedDevelopmentSkill> {
    let registry = read_development_registry(workspace_root)?;
    let registration = find_registered_skill(&registry, skill_id)?.clone();
    build_resolved_development_skill(workspace_root, registration)
}

pub fn resolve_development_skill_if_registered(
    workspace_root: &Path,
    skill_id: &str,
) -> Result<Option<ResolvedDevelopmentSkill>> {
    if !development_registry_path(workspace_root).is_file() {
        return Ok(None);
    }
    let registry = read_development_registry(workspace_root)?;
    let Some(registration) = registry
        .skills
        .into_iter()
        .find(|registration| registration.skill_id == skill_id)
    else {
        return Ok(None);
    };
    Ok(Some(build_resolved_development_skill(
        workspace_root,
        registration,
    )?))
}

fn build_resolved_development_skill(
    workspace_root: &Path,
    registration: DevelopmentSkillRegistration,
) -> Result<ResolvedDevelopmentSkill> {
    let bundle_root = resolve_registered_source_root(workspace_root, &registration);
    let descriptor = read_descriptor(&bundle_root)?;
    if descriptor.skill_id != registration.skill_id {
        bail!(
            "development registry maps {} to {} but descriptor declares {}",
            registration.skill_id,
            bundle_root.display(),
            descriptor.skill_id
        );
    }
    if descriptor.loader_id != registration.loader_id {
        bail!(
            "development registry maps {} to loader {} but descriptor declares {}",
            registration.skill_id,
            registration.loader_id,
            descriptor.loader_id
        );
    }
    if descriptor.internal_manifest != registration.internal_manifest {
        bail!(
            "development registry maps {} to manifest {} but descriptor declares {}",
            registration.skill_id,
            registration.internal_manifest,
            descriptor.internal_manifest
        );
    }
    Ok(ResolvedDevelopmentSkill {
        bundle_root,
        descriptor,
        registration,
    })
}

pub fn discover_bundle_from_binary_path(binary_path: &Path) -> Result<Option<DiscoveredBundle>> {
    let Some(bin_dir) = binary_path.parent() else {
        return Ok(None);
    };
    if bin_dir.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return Ok(None);
    }
    let Some(bundle_root) = bin_dir.parent() else {
        return Ok(None);
    };
    let descriptor_path = descriptor_path(bundle_root);
    if !descriptor_path.is_file() {
        return Ok(None);
    }
    let descriptor = read_descriptor(bundle_root)?;
    let expected_binary_path = bundle_root.join(&descriptor.binary_path);
    if expected_binary_path != binary_path {
        return Ok(None);
    }
    Ok(Some(DiscoveredBundle {
        bundle_root: bundle_root.to_path_buf(),
        descriptor,
    }))
}

pub fn discover_installed_skill(
    skill_id: &str,
    candidate_roots: &[PathBuf],
) -> Result<DiscoveredBundle> {
    for root in candidate_roots {
        if !root.is_dir() {
            continue;
        }
        for entry in
            std::fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))?
        {
            let entry = entry?;
            let bundle_root = entry.path();
            let descriptor_path = descriptor_path(&bundle_root);
            if !descriptor_path.is_file() {
                continue;
            }
            let descriptor = read_descriptor(&bundle_root)?;
            if descriptor.skill_id == skill_id {
                return Ok(DiscoveredBundle {
                    bundle_root,
                    descriptor,
                });
            }
        }
    }
    bail!("failed to discover installed skill {skill_id}")
}

pub fn discover_installed_skill_in_default_roots(skill_id: &str) -> Result<DiscoveredBundle> {
    discover_installed_skill(skill_id, &host_default_installed_skill_roots())
}

pub fn host_default_installed_skill_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(codex_home) = env::var_os("CODEX_HOME") {
        push_unique_path(&mut roots, PathBuf::from(codex_home).join("skills"));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_path(&mut roots, home.join(".codex").join("skills"));
        push_unique_path(&mut roots, home.join(".claude").join("skills"));
    }
    roots
}

pub fn dispatch_loader<'a, T>(
    loader_id: &str,
    registrations: &'a [LoaderRegistration<T>],
) -> Result<&'a T> {
    registrations
        .iter()
        .find(|registration| registration.loader_id == loader_id)
        .map(|registration| &registration.loader)
        .ok_or_else(|| anyhow!("no loader registered for {loader_id}"))
}

pub fn development_registry_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(DEV_REGISTRY_RELATIVE_PATH)
}

pub fn descriptor_path(bundle_root: &Path) -> PathBuf {
    bundle_root.join(DESCRIPTOR_FILE_NAME)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|path| path == &candidate) {
        paths.push(candidate);
    }
}
