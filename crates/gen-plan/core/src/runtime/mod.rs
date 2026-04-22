use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use loopy_common_bundle::{
    discover_bundle_from_binary_path, discover_installed_skill_in_default_roots,
    resolve_development_skill_if_registered,
};
use loopy_gen_plan_bundle::ResolvedGateRoleSelection;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{
    EnsureNodeIdRequest, EnsureNodeIdResponse, EnsurePlanRequest, EnsurePlanResponse,
    InspectNodeRequest, InspectNodeResponse, ListChildrenRequest, ListChildrenResponse,
    OpenPlanRequest, OpenPlanResponse, RunFrontierReviewGateRequest, RunFrontierReviewGateResponse,
    RunLeafReviewGateRequest, RunLeafReviewGateResponse,
};

mod db;
mod gates;
mod query;

const FIXED_PLANS_RELATIVE_PATH: &str = ".loopy/plans";

#[derive(Debug, Clone)]
pub struct Runtime {
    workspace_root: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedSkillBundle {
    pub bundle_root: PathBuf,
    pub bundle_bin: PathBuf,
}

impl Runtime {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        let runtime = Self {
            workspace_root: workspace_root.into(),
        };
        runtime.bootstrap_filesystem()?;
        Ok(runtime)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn ensure_plan(&self, request: EnsurePlanRequest) -> Result<EnsurePlanResponse> {
        let plan_name = validate_plan_name(&request.plan_name)?;
        let plan_root = self.plan_root(plan_name);
        fs::create_dir_all(&plan_root)
            .with_context(|| format!("failed to create {}", plan_root.display()))?;

        let connection = self.open_connection()?;
        query::ensure_plan(&connection, self.workspace_root(), &plan_root, request)
    }

    pub fn open_plan(&self, request: OpenPlanRequest) -> Result<OpenPlanResponse> {
        let plan_name = validate_plan_name(&request.plan_name)?;
        let plan_root = self.plan_root(plan_name);
        let connection = self.open_connection()?;
        query::open_plan(&connection, self.workspace_root(), &plan_root, request)
    }

    pub fn ensure_node_id(&self, request: EnsureNodeIdRequest) -> Result<EnsureNodeIdResponse> {
        let connection = self.open_connection()?;
        query::ensure_node_id(&connection, request)
    }

    pub fn inspect_node(&self, request: InspectNodeRequest) -> Result<InspectNodeResponse> {
        let connection = self.open_connection()?;
        query::inspect_node(&connection, request)
    }

    pub fn list_children(&self, request: ListChildrenRequest) -> Result<ListChildrenResponse> {
        let connection = self.open_connection()?;
        query::list_children(&connection, request)
    }

    pub fn resolve_gate_roles(&self, plan_id: &str) -> Result<ResolvedGateRoleSelection> {
        let connection = self.open_connection()?;
        let task_type: String = connection
            .query_row(
                "SELECT task_type FROM GEN_PLAN__plans WHERE plan_id = ?1",
                params![plan_id],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read persisted plan task_type")?
            .ok_or_else(|| anyhow!("plan `{plan_id}` does not exist"))?;
        let skill_bundle = self.resolved_skill_bundle()?;
        let manifest = loopy_gen_plan_bundle::load_manifest(&skill_bundle.bundle_root)?;
        loopy_gen_plan_bundle::resolve_gate_roles(&skill_bundle.bundle_root, &manifest, &task_type)
    }

    pub fn run_leaf_review_gate(
        &self,
        request: RunLeafReviewGateRequest,
    ) -> Result<RunLeafReviewGateResponse> {
        let connection = self.open_connection()?;
        gates::run_leaf_review_gate(self, &connection, request)
    }

    pub fn run_frontier_review_gate(
        &self,
        request: RunFrontierReviewGateRequest,
    ) -> Result<RunFrontierReviewGateResponse> {
        let connection = self.open_connection()?;
        gates::run_frontier_review_gate(self, &connection, request)
    }

    fn bootstrap_filesystem(&self) -> Result<()> {
        let loopy_dir = self.workspace_root.join(".loopy");
        fs::create_dir_all(&loopy_dir)
            .with_context(|| format!("failed to create {}", loopy_dir.display()))?;

        let plans_dir = self.workspace_root.join(FIXED_PLANS_RELATIVE_PATH);
        fs::create_dir_all(&plans_dir)
            .with_context(|| format!("failed to create {}", plans_dir.display()))?;
        Ok(())
    }

    fn open_connection(&self) -> Result<Connection> {
        self.bootstrap_filesystem()?;

        let db_path = self.db_path();
        let connection = Connection::open(&db_path)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        loopy_common_sqlite::configure_write_connection(&connection)?;
        db::bootstrap_schema(&connection)?;
        Ok(connection)
    }

    fn db_path(&self) -> PathBuf {
        self.workspace_root.join(db::FIXED_DB_RELATIVE_PATH)
    }

    fn plan_root(&self, plan_name: &str) -> PathBuf {
        self.workspace_root
            .join(FIXED_PLANS_RELATIVE_PATH)
            .join(plan_name)
    }

    pub(crate) fn resolved_skill_bundle(&self) -> Result<ResolvedSkillBundle> {
        if let Some(bundle_root) = discover_current_process_bundle()? {
            return resolved_bundle_from_root(bundle_root);
        }
        if let Some(development_skill) = resolve_development_skill_if_registered(
            &self.workspace_root,
            loopy_gen_plan_bundle::SKILL_ID,
        )? {
            loopy_gen_plan_bundle::load_bundle_descriptor(&development_skill.bundle_root)?;
            return resolved_bundle_from_root(development_skill.bundle_root);
        }
        let installed_skill =
            discover_installed_skill_in_default_roots(loopy_gen_plan_bundle::SKILL_ID)?;
        loopy_gen_plan_bundle::load_bundle_descriptor(&installed_skill.bundle_root)?;
        resolved_bundle_from_root(installed_skill.bundle_root)
    }
}

fn discover_current_process_bundle() -> Result<Option<PathBuf>> {
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let Some(discovered_bundle) = discover_bundle_from_binary_path(&current_exe)? else {
        return Ok(None);
    };
    loopy_gen_plan_bundle::load_bundle_descriptor(&discovered_bundle.bundle_root)?;
    Ok(Some(discovered_bundle.bundle_root))
}

fn resolved_bundle_from_root(bundle_root: PathBuf) -> Result<ResolvedSkillBundle> {
    let descriptor = loopy_gen_plan_bundle::load_bundle_descriptor(&bundle_root)?;
    let bundle_bin = bundle_root.join(descriptor.binary_path);
    Ok(ResolvedSkillBundle {
        bundle_root,
        bundle_bin,
    })
}

fn validate_plan_name(plan_name: &str) -> Result<&str> {
    if plan_name.is_empty() {
        bail!("plan_name must not be empty");
    }

    let path = Path::new(plan_name);
    if path.is_absolute() {
        bail!("plan_name must stay within .loopy/plans/: absolute paths are not allowed");
    }

    let mut components = path.components();
    let Some(Component::Normal(component)) = components.next() else {
        bail!("plan_name must be a single directory name within .loopy/plans/");
    };
    if components.next().is_some() {
        bail!("plan_name must be a single directory name within .loopy/plans/");
    }

    if component != plan_name {
        bail!("plan_name must be normalized within .loopy/plans/");
    }

    Ok(plan_name)
}
