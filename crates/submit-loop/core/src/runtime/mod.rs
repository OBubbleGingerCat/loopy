// Runtime owns the public facade and shared internal state; extracted business logic lives in submodules.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use loopy_common_bundle::{
    BundleDescriptor, LoaderRegistration, ResolvedDevelopmentSkill,
    discover_bundle_from_binary_path, discover_installed_skill_in_default_roots, dispatch_loader,
    read_descriptor, resolve_development_skill_if_registered,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const FIXED_DB_RELATIVE_PATH: &str = "./.loopy/loopy.db";

mod api;

pub(crate) mod ops;
pub(crate) mod projection;
pub(crate) mod query;
pub(crate) mod roles;
pub(crate) mod system;

pub use self::api::*;
pub(crate) use loopy_submit_loop_bundle::ResolvedRoleSelection;

#[derive(Debug, Clone)]
pub struct Runtime {
    workspace_root: PathBuf,
    db_path_override: Option<PathBuf>,
    installed_skill_root_override: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct ResolvedSkillBundle {
    bundle_root: PathBuf,
    bundle_bin: PathBuf,
}

impl Runtime {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_overrides(workspace_root, None, None)
    }

    pub fn with_installed_skill_root(
        workspace_root: impl Into<PathBuf>,
        installed_skill_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::with_overrides(workspace_root, None, Some(installed_skill_root.into()))
    }

    pub fn with_db_path_override(
        workspace_root: impl Into<PathBuf>,
        db_path_override: Option<PathBuf>,
    ) -> Result<Self> {
        Self::with_overrides(workspace_root, db_path_override, None)
    }

    fn with_overrides(
        workspace_root: impl Into<PathBuf>,
        db_path_override: Option<PathBuf>,
        installed_skill_root_override: Option<PathBuf>,
    ) -> Result<Self> {
        let runtime = Self {
            workspace_root: workspace_root.into(),
            db_path_override,
            installed_skill_root_override,
        };
        runtime.validate_db_path()?;
        Ok(runtime)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn db_path_override(&self) -> Option<&Path> {
        self.db_path_override.as_deref()
    }

    pub fn installed_skill_root(&self) -> Result<PathBuf> {
        Ok(self.resolved_skill_bundle()?.bundle_root)
    }

    pub(crate) fn bundle_binary_path(&self) -> Result<PathBuf> {
        Ok(self.resolved_skill_bundle()?.bundle_bin)
    }

    pub fn from_invocation_context_path(invocation_context_path: &Path) -> Result<Self> {
        let invocations_dir = invocation_context_path
            .parent()
            .ok_or_else(|| anyhow!("invalid invocation context path"))?;
        let loopy_dir = invocations_dir
            .parent()
            .ok_or_else(|| anyhow!("invalid invocation context path"))?;
        if invocations_dir.file_name().and_then(|name| name.to_str()) != Some("invocations")
            || loopy_dir.file_name().and_then(|name| name.to_str()) != Some(".loopy")
        {
            bail!(
                "invocation context path must live under <workspace>/.loopy/invocations: {}",
                invocation_context_path.display()
            );
        }
        let workspace_root = loopy_dir
            .parent()
            .ok_or_else(|| anyhow!("invalid invocation context path"))?;
        Self::new(workspace_root)
    }

    pub fn open_loop(&self, request: OpenLoopRequest) -> Result<OpenLoopResponse> {
        ops::r#loop::open_loop(self, request)
    }

    pub fn rebuild_projections(&self) -> Result<()> {
        self.rebuild_all_projections()
    }

    pub fn rebuild_all_projections(&self) -> Result<()> {
        let mut connection = self.open_connection()?;
        let transaction = begin_immediate_transaction(&mut connection)?;
        projection::rebuild_all_projections(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn rebuild_loop_projections(&self, loop_id: &str) -> Result<()> {
        let mut connection = self.open_connection()?;
        let transaction = begin_immediate_transaction(&mut connection)?;
        projection::rebuild_single_loop_projections(&transaction, loop_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn show_loop(&self, request: ShowLoopRequest) -> Result<ShowLoopSummary> {
        query::show_loop(self, request)
    }

    pub fn handoff_to_caller_finalize(
        &self,
        request: HandoffToCallerFinalizeRequest,
    ) -> Result<HandoffToCallerFinalizeResponse> {
        ops::caller_finalize::handoff_to_caller_finalize(self, request)
    }

    pub fn begin_caller_finalize(
        &self,
        request: BeginCallerFinalizeRequest,
    ) -> Result<BeginCallerFinalizeResponse> {
        ops::caller_finalize::begin_caller_finalize(self, request)
    }

    pub fn block_caller_finalize(
        &self,
        request: BlockCallerFinalizeRequest,
    ) -> Result<BlockCallerFinalizeResponse> {
        ops::caller_finalize::block_caller_finalize(self, request)
    }

    pub fn prepare_worktree(
        &self,
        request: PrepareWorktreeRequest,
    ) -> Result<PrepareWorktreeResponse> {
        ops::r#loop::prepare_worktree(self, request)
    }

    pub fn open_review_round(
        &self,
        request: OpenReviewRoundRequest,
    ) -> Result<OpenReviewRoundResponse> {
        ops::r#loop::open_review_round(self, request)
    }

    pub fn start_worker_invocation(
        &self,
        request: StartWorkerInvocationRequest,
    ) -> Result<StartWorkerInvocationResponse> {
        ops::invocation::start_worker_invocation(self, request)
    }

    pub fn start_reviewer_invocation(
        &self,
        request: StartReviewerInvocationRequest,
    ) -> Result<StartReviewerInvocationResponse> {
        ops::invocation::start_reviewer_invocation(self, request)
    }

    pub fn submit_checkpoint_plan(
        &self,
        request: SubmitCheckpointPlanRequest,
    ) -> Result<SubmitCheckpointPlanResponse> {
        ops::submissions::submit_checkpoint_plan(self, request)
    }

    pub fn submit_checkpoint_review(
        &self,
        request: SubmitCheckpointReviewRequest,
    ) -> Result<SubmitCheckpointReviewResponse> {
        ops::submissions::submit_checkpoint_review(self, request)
    }

    pub fn submit_artifact_review(
        &self,
        request: SubmitArtifactReviewRequest,
    ) -> Result<SubmitArtifactReviewResponse> {
        ops::submissions::submit_artifact_review(self, request)
    }

    pub fn request_timeout_extension(
        &self,
        request: RequestTimeoutExtensionRequest,
    ) -> Result<RequestTimeoutExtensionResponse> {
        ops::submissions::request_timeout_extension(self, request)
    }

    pub fn submit_candidate_commit(
        &self,
        request: SubmitCandidateCommitRequest,
    ) -> Result<SubmitCandidateCommitResponse> {
        ops::submissions::submit_candidate_commit(self, request)
    }

    pub fn declare_worker_blocked(
        &self,
        request: DeclareWorkerBlockedRequest,
    ) -> Result<DeclareWorkerBlockedResponse> {
        ops::submissions::declare_worker_blocked(self, request)
    }

    pub fn declare_review_blocked(
        &self,
        request: DeclareReviewBlockedRequest,
    ) -> Result<DeclareReviewBlockedResponse> {
        ops::submissions::declare_review_blocked(self, request)
    }

    pub fn finalize_failure(
        &self,
        request: FinalizeFailureRequest,
    ) -> Result<FinalizeFailureResponse> {
        ops::r#loop::finalize_failure(self, request)
    }

    pub fn finalize_success(
        &self,
        request: FinalizeSuccessRequest,
    ) -> Result<FinalizeSuccessResponse> {
        ops::caller_finalize::finalize_success(self, request)
    }

    fn open_connection(&self) -> Result<Connection> {
        let db_path = self.db_path()?;
        let connection = Connection::open(&db_path)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        configure_write_connection(&connection)?;
        if projection::schema_bootstrap_required(&connection)? {
            projection::bootstrap_schema(&connection)?;
        }
        Ok(connection)
    }

    fn open_read_only_connection(&self) -> Result<Connection> {
        let db_path = self.db_path()?;
        let connection = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        configure_read_only_connection(&connection)?;
        Ok(connection)
    }

    fn db_path(&self) -> Result<PathBuf> {
        self.validate_db_path()?;
        Ok(self
            .db_path_override
            .clone()
            .unwrap_or_else(|| self.expected_db_path()))
    }

    fn expected_db_path(&self) -> PathBuf {
        self.workspace_root.join(".loopy").join("loopy.db")
    }

    fn validate_db_path(&self) -> Result<()> {
        if let Some(override_path) = &self.db_path_override {
            let normalized_override = if override_path.is_absolute() {
                override_path.clone()
            } else {
                self.workspace_root.join(override_path)
            };
            let expected = self.expected_db_path();
            if normalized_override != expected {
                bail!(
                    "authoritative runtime state must live at {} (expected {}, got {})",
                    FIXED_DB_RELATIVE_PATH,
                    expected.display(),
                    normalized_override.display()
                );
            }
        }
        Ok(())
    }

    fn resolved_skill_bundle(&self) -> Result<ResolvedSkillBundle> {
        if let Some(override_root) = &self.installed_skill_root_override {
            let descriptor = read_descriptor(override_root)?;
            let bundle_bin = override_root.join(&descriptor.binary_path);
            return resolved_bundle_from_descriptor(override_root.clone(), descriptor, bundle_bin);
        }
        if let Some(bundle) = discover_current_process_bundle()? {
            return Ok(bundle);
        }
        if let Some(development_skill) = resolve_development_skill_if_registered(
            &self.workspace_root,
            loopy_submit_loop_bundle::SKILL_ID,
        )? {
            return resolve_development_skill_bundle(&self.workspace_root, development_skill);
        }
        let installed_skill =
            discover_installed_skill_in_default_roots(loopy_submit_loop_bundle::SKILL_ID)?;
        let bundle_root = installed_skill.bundle_root;
        let descriptor = installed_skill.descriptor;
        let bundle_bin = bundle_root.join(&descriptor.binary_path);
        resolved_bundle_from_descriptor(bundle_root, descriptor, bundle_bin)
    }
}

pub(crate) fn begin_immediate_transaction(connection: &mut Connection) -> Result<Transaction<'_>> {
    loopy_common_sqlite::begin_immediate_transaction(connection)
}

fn configure_write_connection(connection: &Connection) -> Result<()> {
    loopy_common_sqlite::configure_write_connection(connection)
}

fn configure_read_only_connection(connection: &Connection) -> Result<()> {
    loopy_common_sqlite::configure_read_only_connection(connection)
}

type BundleValidator = fn(&Path) -> Result<()>;

fn validate_submit_loop_bundle_descriptor(bundle_root: &Path) -> Result<()> {
    loopy_submit_loop_bundle::load_bundle_descriptor(bundle_root).map(|_| ())
}

fn resolved_bundle_from_descriptor(
    bundle_root: PathBuf,
    descriptor: BundleDescriptor,
    bundle_bin: PathBuf,
) -> Result<ResolvedSkillBundle> {
    validate_submit_loop_loader(&bundle_root, &descriptor)?;
    Ok(ResolvedSkillBundle {
        bundle_root,
        bundle_bin,
    })
}

fn validate_submit_loop_loader(bundle_root: &Path, descriptor: &BundleDescriptor) -> Result<()> {
    let registrations = [LoaderRegistration {
        loader_id: loopy_submit_loop_bundle::LOADER_ID,
        loader: validate_submit_loop_bundle_descriptor as BundleValidator,
    }];
    let validate_bundle = dispatch_loader(&descriptor.loader_id, &registrations)?;
    validate_bundle(bundle_root)?;
    if descriptor.skill_id != loopy_submit_loop_bundle::SKILL_ID {
        bail!(
            "expected skill_id {} in {}, found {}",
            loopy_submit_loop_bundle::SKILL_ID,
            bundle_root.display(),
            descriptor.skill_id
        );
    }
    Ok(())
}

fn discover_current_process_bundle() -> Result<Option<ResolvedSkillBundle>> {
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let Some(discovered_bundle) = discover_bundle_from_binary_path(&current_exe)? else {
        return Ok(None);
    };
    Ok(Some(resolved_bundle_from_descriptor(
        discovered_bundle.bundle_root,
        discovered_bundle.descriptor,
        current_exe,
    )?))
}

fn resolve_development_skill_bundle(
    workspace_root: &Path,
    development_skill: ResolvedDevelopmentSkill,
) -> Result<ResolvedSkillBundle> {
    let bundle_bin = resolve_development_bundle_binary(workspace_root, &development_skill)?;
    resolved_bundle_from_descriptor(
        development_skill.bundle_root,
        development_skill.descriptor,
        bundle_bin,
    )
}

fn resolve_development_bundle_binary(
    workspace_root: &Path,
    development_skill: &ResolvedDevelopmentSkill,
) -> Result<PathBuf> {
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    if current_exe.file_name().and_then(|name| name.to_str())
        == Some(development_skill.registration.binary_name.as_str())
    {
        return Ok(current_exe);
    }

    if let Some(profile_binary) =
        current_profile_binary_candidate(&current_exe, &development_skill.registration.binary_name)
    {
        if profile_binary.is_file() {
            return Ok(profile_binary);
        }
    }

    for profile in ["debug", "release"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join(&development_skill.registration.binary_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let bundled_binary = development_skill
        .bundle_root
        .join(&development_skill.descriptor.binary_path);
    if bundled_binary.is_file() {
        return Ok(bundled_binary);
    }

    bail!(
        "failed to resolve development executable {} for cargo package {} from {}",
        development_skill.registration.binary_name,
        development_skill.registration.binary_package,
        workspace_root.display()
    )
}

fn current_profile_binary_candidate(current_exe: &Path, binary_name: &str) -> Option<PathBuf> {
    let current_dir = current_exe.parent()?;
    let profile_dir = if current_dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
        current_dir.parent()?
    } else {
        current_dir
    };
    Some(profile_dir.join(binary_name))
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

#[derive(Debug)]
pub(crate) struct ShowLoopCoreState {
    loop_id: String,
    status: String,
    phase: String,
    updated_at: String,
}

#[derive(Debug)]
pub(crate) struct LoopState {
    phase: String,
    status: String,
    base_commit_sha: String,
    loop_input_ref: String,
    resolved_role_selection_ref: String,
    worktree_path: String,
    worktree_branch: String,
    worktree_label: String,
    failure_summary: Option<String>,
}

pub(crate) struct InvocationDispatchState {
    loop_id: String,
    invocation_context_ref: String,
    executor_config_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewSlotState {
    review_slot_id: String,
    #[serde(default)]
    reviewer_role_id: Option<String>,
    status: String,
    decision: Option<String>,
    submission_content_ref: Option<String>,
}

pub(crate) struct ReviewRoundState {
    review_kind: String,
    target_type: String,
    target_ref: String,
    target_metadata: Value,
    slot_state: Vec<ReviewSlotState>,
}

pub(crate) struct InvocationState {
    loop_id: String,
    invocation_role: String,
    stage: String,
    status: String,
    token: String,
    token_state: String,
    accepted_api: Option<String>,
    accepted_submission_id: Option<String>,
    invocation_context_ref: String,
    allowed_terminal_apis: Vec<String>,
    review_round_id: Option<String>,
    review_slot_id: Option<String>,
}

pub(crate) struct FailureEventState {
    event_id: i64,
    failure_cause_type: String,
    summary: String,
    phase_at_failure: String,
    last_stable_context: Value,
}

pub(crate) struct WorktreeCleanupWarningState {
    summary: String,
    worktree_path: String,
    worktree_branch: String,
    worktree_label: String,
}

pub(crate) struct CheckpointState {
    checkpoint_id: String,
    sequence_index: i64,
    title: String,
    kind: String,
    deliverables: Vec<CheckpointDeliverable>,
    acceptance: CheckpointAcceptance,
    execution_state: String,
    accepted_commit_sha: Option<String>,
    candidate_commit_sha: Option<String>,
}

pub(crate) struct CandidateCommitState {
    checkpoint_id: String,
    title: String,
    commit_sha: String,
    change_summary: Value,
}

pub(crate) struct AcceptedArtifactMaterial {
    title: String,
    change_summary: Value,
}

pub(crate) struct CallerIntegrationState {
    caller_branch: String,
    final_head_sha: String,
    strategy: String,
    landed_commit_shas: Vec<String>,
    resolution_notes: Option<String>,
}

pub(crate) struct AuthenticatedTerminalRequest {
    loop_id: String,
    invocation_id: String,
    invocation_state: InvocationState,
    stored_invocation_context: Value,
}

pub(crate) struct TimeoutExtensionRequestState {
    request_content_ref: String,
    requested_timeout_sec: i64,
    progress_summary: String,
    rationale: String,
}

pub(crate) enum ReviewSlotTerminal {
    Decision {
        decision: String,
        submission_content_ref: String,
    },
    Blocked {
        submission_content_ref: String,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::Result;
    use loopy_common_bundle::{
        DevelopmentSkillRegistration, ResolvedDevelopmentSkill, read_descriptor,
    };

    use super::{Runtime, current_profile_binary_candidate, resolve_development_bundle_binary};

    #[test]
    fn resolved_skill_bundle_uses_real_binary_path_for_dev_registry_source_roots() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let source_root = submit_loop_source_root();
        write_submit_loop_dev_registry(workspace.path(), &source_root)?;

        let runtime = Runtime::new(workspace.path())?;
        let resolved_bundle = runtime.resolved_skill_bundle()?;

        assert_eq!(resolved_bundle.bundle_root, source_root);
        assert!(
            resolved_bundle.bundle_bin.is_file(),
            "expected development bundle binary at {}",
            resolved_bundle.bundle_bin.display()
        );
        assert_eq!(
            resolved_bundle
                .bundle_bin
                .file_name()
                .and_then(|name| name.to_str()),
            Some("loopy-submit-loop")
        );
        assert_ne!(
            resolved_bundle.bundle_bin,
            resolved_bundle
                .bundle_root
                .join("bin")
                .join("loopy-submit-loop"),
            "development resolution must not point dispatches at an unbuilt source-tree bin path"
        );

        Ok(())
    }

    #[test]
    fn development_bundle_binary_prefers_built_binary_over_source_tree_bundle_copy() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let source_root = tempfile::tempdir()?;
        write_submit_loop_bundle_descriptor(source_root.path())?;

        let bundled_binary = source_root.path().join("bin").join("loopy-submit-loop");
        fs::create_dir_all(
            bundled_binary
                .parent()
                .expect("bundled binary should have a parent"),
        )?;
        fs::write(&bundled_binary, "#!/bin/sh\nexit 0\n")?;

        let workspace_binary = workspace
            .path()
            .join("target")
            .join("debug")
            .join("loopy-submit-loop");
        fs::create_dir_all(
            workspace_binary
                .parent()
                .expect("workspace binary should have a parent"),
        )?;
        fs::write(&workspace_binary, "#!/bin/sh\nexit 0\n")?;

        let descriptor = read_descriptor(source_root.path())?;
        let development_skill = ResolvedDevelopmentSkill {
            bundle_root: source_root.path().to_path_buf(),
            descriptor,
            registration: DevelopmentSkillRegistration {
                skill_id: "loopy:submit-loop".to_owned(),
                loader_id: "loopy.submit-loop.v1".to_owned(),
                source_root: source_root.path().display().to_string(),
                binary_package: "loopy-submit-loop".to_owned(),
                binary_name: "loopy-submit-loop".to_owned(),
                internal_manifest: "submit-loop.toml".to_owned(),
            },
        };
        let current_exe = std::env::current_exe()?;
        let expected_binary = current_profile_binary_candidate(&current_exe, "loopy-submit-loop")
            .filter(|candidate| candidate.is_file())
            .unwrap_or(workspace_binary);

        let resolved_binary =
            resolve_development_bundle_binary(workspace.path(), &development_skill)?;

        assert_eq!(resolved_binary, expected_binary);
        assert_ne!(
            resolved_binary, bundled_binary,
            "development resolution should prefer the active built binary over a stray source-tree copy"
        );

        Ok(())
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("core crate should live under <repo>/crates/submit-loop/core")
            .to_path_buf()
    }

    fn submit_loop_source_root() -> PathBuf {
        repo_root().join("skills").join("submit-loop")
    }

    fn write_submit_loop_dev_registry(workspace_root: &Path, source_root: &Path) -> Result<()> {
        let registry_dir = workspace_root.join("skills");
        fs::create_dir_all(&registry_dir)?;
        let registry_path = registry_dir.join("dev-registry.toml");
        fs::write(
            &registry_path,
            format!(
                "[[skills]]\nskill_id = \"loopy:submit-loop\"\nloader_id = \"loopy.submit-loop.v1\"\nsource_root = \"{}\"\nbinary_package = \"loopy-submit-loop\"\nbinary_name = \"loopy-submit-loop\"\ninternal_manifest = \"submit-loop.toml\"\n",
                source_root.display()
            ),
        )?;
        Ok(())
    }

    fn write_submit_loop_bundle_descriptor(bundle_root: &Path) -> Result<()> {
        fs::create_dir_all(bundle_root)?;
        fs::write(
            bundle_root.join("bundle.toml"),
            [
                "skill_id = \"loopy:submit-loop\"",
                "skill_kind = \"bundle\"",
                "version = \"0.1.0\"",
                "loader_id = \"loopy.submit-loop.v1\"",
                "root_entry = \"SKILL.md\"",
                "binary_path = \"bin/loopy-submit-loop\"",
                "internal_manifest = \"submit-loop.toml\"",
                "",
            ]
            .join("\n"),
        )?;
        Ok(())
    }
}
