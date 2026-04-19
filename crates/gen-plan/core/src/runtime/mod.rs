use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::{
    EnsureNodeIdRequest, EnsureNodeIdResponse, EnsurePlanRequest, EnsurePlanResponse,
    OpenPlanRequest, OpenPlanResponse,
};

mod db;
mod query;

const FIXED_PLANS_RELATIVE_PATH: &str = ".loopy/plans";

#[derive(Debug, Clone)]
pub struct Runtime {
    workspace_root: PathBuf,
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
