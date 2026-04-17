use std::path::Path;

use anyhow::{Result, bail};
use loopy_common_bundle::read_descriptor;

pub const SKILL_ID: &str = "loopy:gen-plan";
pub const LOADER_ID: &str = "loopy.gen-plan.v1";

pub fn validate_placeholder_bundle(bundle_root: &Path) -> Result<()> {
    let descriptor = read_descriptor(bundle_root)?;
    if descriptor.skill_id != SKILL_ID || descriptor.loader_id != LOADER_ID {
        bail!("unexpected descriptor for gen-plan placeholder bundle");
    }
    Ok(())
}
