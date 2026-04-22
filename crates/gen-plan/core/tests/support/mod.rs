use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

pub struct TestWorkspace {
    path: PathBuf,
}

impl TestWorkspace {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn workspace() -> Result<TestWorkspace> {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!(
        "loopy-gen-plan-runtime-kernel-{}-{}",
        std::process::id(),
        unique
    ));
    fs::create_dir_all(&path)?;
    Ok(TestWorkspace { path })
}

#[allow(dead_code)]
pub fn assert_dir_exists(path: &Path) {
    assert!(path.is_dir(), "expected directory at {}", path.display());
}
