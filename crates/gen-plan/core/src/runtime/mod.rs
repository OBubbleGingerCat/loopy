use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Runtime {
    workspace_root: PathBuf,
}

impl Runtime {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            workspace_root: workspace_root.into(),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}
