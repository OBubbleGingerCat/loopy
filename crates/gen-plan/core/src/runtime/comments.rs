use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};

const BEGIN_COMMENT: &str = "BEGIN_COMMENT";
const END_COMMENT: &str = "END_COMMENT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBlock {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentBlockParseError {
    NestedBegin { line: usize, open_start_line: usize },
    OrphanEnd { line: usize },
    UnclosedBegin { start_line: usize },
}

impl CommentBlockParseError {
    pub fn marker_line(&self) -> usize {
        match self {
            Self::NestedBegin { line, .. } | Self::OrphanEnd { line } => *line,
            Self::UnclosedBegin { start_line } => *start_line,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::NestedBegin { .. } => "nested_begin_comment",
            Self::OrphanEnd { .. } => "orphan_end_comment",
            Self::UnclosedBegin { .. } => "unclosed_begin_comment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentDiscoveryError {
    InvalidPlanRoot {
        plan_root: PathBuf,
    },
    Io {
        relative_path: String,
        source: String,
    },
    MalformedStructure {
        relative_path: String,
        line: usize,
        kind: String,
        message: String,
    },
}

impl fmt::Display for CommentDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlanRoot { plan_root } => {
                write!(formatter, "invalid plan root `{}`", plan_root.display())
            }
            Self::Io {
                relative_path,
                source,
            } => {
                write!(formatter, "failed to read `{relative_path}`: {source}")
            }
            Self::MalformedStructure {
                relative_path,
                line,
                kind,
                message,
            } => {
                write!(
                    formatter,
                    "malformed refine comment in `{relative_path}` at line {line} ({kind}): {message}"
                )
            }
        }
    }
}

impl std::error::Error for CommentDiscoveryError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedComment {
    pub relative_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

pub fn collect_plan_markdown_files(plan_root: &Path, plan_name: &str) -> Result<Vec<String>> {
    let metadata = fs::metadata(plan_root)
        .with_context(|| format!("failed to inspect {}", plan_root.display()))?;
    if !metadata.is_dir() {
        return Err(anyhow!(
            "plan root {} is not a directory",
            plan_root.display()
        ));
    }

    let draft_file_name = format!("{plan_name}_draft.md");
    let mut files = Vec::new();
    collect_markdown_files_recursive(plan_root, plan_root, &draft_file_name, &mut files)?;
    files.sort();
    Ok(files)
}

pub fn parse_comment_blocks(
    markdown: &str,
) -> std::result::Result<Vec<CommentBlock>, CommentBlockParseError> {
    let mut blocks = Vec::new();
    let mut open_start_line = None;
    let mut open_text = Vec::<String>::new();

    for (index, line) in markdown.lines().enumerate() {
        let line_number = index + 1;
        match line.trim() {
            BEGIN_COMMENT => {
                if let Some(start_line) = open_start_line {
                    return Err(CommentBlockParseError::NestedBegin {
                        line: line_number,
                        open_start_line: start_line,
                    });
                }
                open_start_line = Some(line_number);
                open_text.clear();
            }
            END_COMMENT => {
                let Some(start_line) = open_start_line.take() else {
                    return Err(CommentBlockParseError::OrphanEnd { line: line_number });
                };
                blocks.push(CommentBlock {
                    start_line,
                    end_line: line_number,
                    text: open_text.join("\n"),
                });
                open_text.clear();
            }
            _ => {
                if open_start_line.is_some() {
                    open_text.push(line.to_owned());
                }
            }
        }
    }

    if let Some(start_line) = open_start_line {
        return Err(CommentBlockParseError::UnclosedBegin { start_line });
    }

    Ok(blocks)
}

pub fn parse_comment_blocks_for_file(
    relative_path: &str,
    markdown: &str,
) -> std::result::Result<Vec<CommentBlock>, CommentDiscoveryError> {
    parse_comment_blocks(markdown).map_err(|error| {
        let message = match &error {
            CommentBlockParseError::NestedBegin {
                line,
                open_start_line,
            } => format!(
                "nested BEGIN_COMMENT at line {line} while block from line {open_start_line} is still open"
            ),
            CommentBlockParseError::OrphanEnd { line } => {
                format!("orphan END_COMMENT at line {line}")
            }
            CommentBlockParseError::UnclosedBegin { start_line } => {
                format!("unclosed BEGIN_COMMENT at line {start_line}")
            }
        };
        CommentDiscoveryError::MalformedStructure {
            relative_path: relative_path.to_owned(),
            line: error.marker_line(),
            kind: error.kind().to_owned(),
            message,
        }
    })
}

pub fn discover_plan_comments(
    plan_root: &Path,
    plan_name: &str,
) -> std::result::Result<Vec<ExtractedComment>, CommentDiscoveryError> {
    let files = collect_plan_markdown_files(plan_root, plan_name).map_err(|_| {
        CommentDiscoveryError::InvalidPlanRoot {
            plan_root: plan_root.to_path_buf(),
        }
    })?;
    let mut comments = Vec::new();
    for relative_path in files {
        let full_path = plan_root.join(&relative_path);
        let markdown =
            fs::read_to_string(&full_path).map_err(|source| CommentDiscoveryError::Io {
                relative_path: relative_path.clone(),
                source: source.to_string(),
            })?;
        for block in parse_comment_blocks_for_file(&relative_path, &markdown)? {
            comments.push(ExtractedComment {
                relative_path: relative_path.clone(),
                start_line: block.start_line,
                end_line: block.end_line,
                text: block.text,
            });
        }
    }
    Ok(comments)
}

fn collect_markdown_files_recursive(
    plan_root: &Path,
    current_dir: &Path,
    draft_file_name: &str,
    files: &mut Vec<String>,
) -> Result<()> {
    let mut entries = fs::read_dir(current_dir)
        .with_context(|| format!("failed to read {}", current_dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", current_dir.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            collect_markdown_files_recursive(plan_root, &path, draft_file_name, files)?;
            continue;
        }
        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if path.parent() == Some(plan_root)
            && path.file_name().and_then(|name| name.to_str()) == Some(draft_file_name)
        {
            continue;
        }
        files.push(plan_relative_path(plan_root, &path)?);
    }
    Ok(())
}

fn plan_relative_path(plan_root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(plan_root).with_context(|| {
        format!(
            "path {} does not stay inside plan root {}",
            path.display(),
            plan_root.display()
        )
    })?;
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            _ => return Err(anyhow!("plan-relative markdown path must be normalized")),
        }
    }
    Ok(normalized.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_comment_parser_extracts_valid_blocks_and_rejects_malformed_markers() {
        let blocks = parse_comment_blocks("a\n BEGIN_COMMENT \nhello\nworld\n END_COMMENT \nz")
            .expect("valid block");
        assert_eq!(
            blocks,
            vec![CommentBlock {
                start_line: 2,
                end_line: 5,
                text: "hello\nworld".to_owned(),
            }]
        );

        assert!(matches!(
            parse_comment_blocks("BEGIN_COMMENT\nBEGIN_COMMENT\nEND_COMMENT"),
            Err(CommentBlockParseError::NestedBegin {
                line: 2,
                open_start_line: 1
            })
        ));
        assert!(matches!(
            parse_comment_blocks("END_COMMENT"),
            Err(CommentBlockParseError::OrphanEnd { line: 1 })
        ));
        assert!(matches!(
            parse_comment_blocks("BEGIN_COMMENT\nstill open"),
            Err(CommentBlockParseError::UnclosedBegin { start_line: 1 })
        ));
    }
}
