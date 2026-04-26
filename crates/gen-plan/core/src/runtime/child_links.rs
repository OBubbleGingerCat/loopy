use std::path::{Component, Path};

use anyhow::{Result, anyhow};

pub(crate) fn parse_child_node_link_paths(
    parent_relative_path: &str,
    markdown: &str,
) -> Result<Vec<String>> {
    let Some((_, body_start, section_end)) = child_nodes_section_offsets(markdown) else {
        return Ok(Vec::new());
    };
    let body = &markdown[body_start..section_end];
    let mut paths = Vec::new();
    for line in body.lines() {
        let Some(url) = parse_markdown_child_link_url(line) else {
            continue;
        };
        let Some(canonical_path) = canonical_child_path_from_url(parent_relative_path, &url)?
        else {
            continue;
        };
        validate_canonical_markdown_path(&canonical_path)?;
        if !paths.contains(&canonical_path) {
            paths.push(canonical_path);
        }
    }
    Ok(paths)
}

fn parse_markdown_child_link_url(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let link_start = trimmed.strip_prefix("- [")?;
    let label_end = link_start.find("](")?;
    let rest = &link_start[label_end + 2..];
    let url_end = rest.find(')')?;
    Some(rest[..url_end].to_owned())
}

fn canonical_child_path_from_url(parent_relative_path: &str, url: &str) -> Result<Option<String>> {
    if url.starts_with('#') || url.contains("://") {
        return Ok(None);
    }
    let url = url
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url);
    let url_path = Path::new(url);
    if url_path.is_absolute() {
        return Err(anyhow!(
            "child link target for `{parent_relative_path}` must be plan-local"
        ));
    }
    let parent_dir = Path::new(parent_relative_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut components = path_components(parent_dir)?;
    for component in url_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_string_lossy().to_string()),
            Component::ParentDir => {
                if components.pop().is_none() {
                    return Err(anyhow!(
                        "child link target for `{parent_relative_path}` escapes the plan"
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "child link target for `{parent_relative_path}` must be plan-local"
                ));
            }
        }
    }
    Ok(Some(components.join("/")))
}

fn child_nodes_section_offsets(markdown: &str) -> Option<(usize, usize, usize)> {
    let mut offset = 0;
    let mut heading_start = None;
    let mut body_start = 0;
    for line in markdown.split_inclusive('\n') {
        if heading_start.is_none() && line.trim() == "## Child Nodes" {
            heading_start = Some(offset);
            body_start = offset + line.len();
        } else if heading_start.is_some() && is_next_top_level_section(line) {
            return Some((heading_start.unwrap(), body_start, offset));
        }
        offset += line.len();
    }
    heading_start.map(|heading_start| (heading_start, body_start, markdown.len()))
}

fn is_next_top_level_section(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("## ") && trimmed.trim() != "## Child Nodes"
}

fn path_components(path: &Path) -> Result<Vec<String>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_string_lossy().to_string()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("child link target must stay within the plan"));
            }
        }
    }
    Ok(components)
}

fn validate_canonical_markdown_path(relative_path: &str) -> Result<()> {
    let path = Path::new(relative_path);
    if relative_path.is_empty()
        || path.is_absolute()
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return Err(anyhow!(
            "child link target must be a canonical markdown relative path"
        ));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(anyhow!("child link target must stay inside the plan"));
        }
    }
    Ok(())
}
