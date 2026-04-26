use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    InspectNodeRequest, ListChildrenRequest, NodeKind, PlannerMode, ReviewIssue,
    RunFrontierReviewGateRequest, RunFrontierReviewGateResponse, RunLeafReviewGateRequest, Runtime,
};

use super::gate_registration::{
    RegisteredRefineFrontierTarget, RegisteredRefineGateTargets, RegisteredRefineLeafTarget,
};
use super::rewrite::RefineRewriteResult;
use super::runtime_state::RefineStaleResultHandoff;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRefineGateRevalidationRequest {
    pub plan_id: String,
    pub plan_root: PathBuf,
    pub planner_mode: PlannerMode,
    pub registered_targets: RegisteredRefineGateTargets,
    pub retry_policy: RefineGateRetryPolicy,
    pub refine_context: RefineGateRevalidationContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateRevalidationContext {
    pub processed_comment_blocks: Vec<RefineGateProcessedCommentBlock>,
    pub stale_result_handoff: Vec<RefineStaleResultHandoff>,
    pub rewrite_result: Option<RefineRewriteResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateProcessedCommentBlock {
    pub relative_path: String,
    pub begin_comment_line: usize,
    pub end_comment_line: usize,
    pub comment_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateRetryPolicy {
    pub max_invocation_retries: u8,
}

impl Default for RefineGateRetryPolicy {
    fn default() -> Self {
        Self {
            max_invocation_retries: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateExecutionReport {
    pub status: RefineGateExecutionStatus,
    pub leaf_attempts: Vec<RefineGateAttempt>,
    pub frontier_attempts: Vec<RefineGateAttempt>,
    pub exhausted_invocation_retries: Vec<RefineGateInvocationFailure>,
    pub blocked_by_review_issues: Vec<ReviewIssue>,
    pub paused_for_user_decision: Option<String>,
    pub invalidated_leaf_node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineGateExecutionStatus {
    Passed,
    BlockedByReviewIssues,
    PausedForUserDecision,
    ExhaustedInvocationRetries,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineGateKind {
    Leaf,
    Frontier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateAttempt {
    pub gate_kind: RefineGateKind,
    pub target_node_id: String,
    pub target_relative_path: String,
    pub attempt_index: u8,
    pub outcome: RefineGateAttemptOutcome,
    pub content_fingerprint: String,
    pub invalidated_leaf_node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefineGateAttemptOutcome {
    Passed {
        gate_run_id: String,
        verdict: String,
        summary: String,
    },
    ReviewIssues {
        verdict: String,
        issues: Vec<ReviewIssue>,
    },
    PauseForUserDecision {
        summary: String,
        issues: Vec<ReviewIssue>,
    },
    InvocationFailure {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineGateInvocationFailure {
    pub gate_kind: RefineGateKind,
    pub target_node_id: String,
    pub attempt_index: u8,
    pub error: String,
    pub content_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefineGateExecutionError {
    InvalidRegisteredTargets { reason: String },
}

impl fmt::Display for RefineGateExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RefineGateExecutionError {}

pub fn run_refine_gate_revalidation(
    runtime: &Runtime,
    request: RunRefineGateRevalidationRequest,
) -> Result<RefineGateExecutionReport, RefineGateExecutionError> {
    // global leaf-before-frontier; must not interleave leaf and frontier execution parent-by-parent.
    // Frontier targets are not invoked until every required leaf target has a passed gate outcome.
    // valid reviewer issues are not invocation failures. Any preflight violation returns
    // RefineGateExecutionError::InvalidRegisteredTargets.
    // Uses Runtime::run_leaf_review_gate, Runtime::run_frontier_review_gate,
    // RunFrontierReviewGateResponse.invalidated_leaf_node_ids, and Runtime::list_children.
    // Fingerprints are lowercase SHA-256 hex digest values over plan_root.join target bytes:
    // leaf\0{relative_path}\0 and frontier\0{parent_relative_path}\0.
    // The API must not inspect .loopy/loopy.db; parent_targets are not dispatched by this API.
    validate_registered_targets(runtime, &request)?;

    let mut report = RefineGateExecutionReport {
        status: RefineGateExecutionStatus::Passed,
        leaf_attempts: Vec::new(),
        frontier_attempts: Vec::new(),
        exhausted_invocation_retries: Vec::new(),
        blocked_by_review_issues: Vec::new(),
        paused_for_user_decision: None,
        invalidated_leaf_node_ids: Vec::new(),
    };

    for target in &request.registered_targets.leaf_targets {
        let status = run_leaf_target(runtime, &request, target, &mut report)?;
        if status != RefineGateExecutionStatus::Passed {
            report.status = status;
            return Ok(report);
        }
    }

    for target in &request.registered_targets.frontier_targets {
        let status = run_frontier_target(runtime, &request, target, &mut report)?;
        if status != RefineGateExecutionStatus::Passed {
            report.status = status;
            return Ok(report);
        }
    }

    // A fully passed report has an empty invalidated_leaf_node_ids list. Approved frontier response
    // with non-empty invalidated leaf ids is invalid under current runtime contract.
    report.status = RefineGateExecutionStatus::Passed;
    Ok(report)
}

fn run_leaf_target(
    runtime: &Runtime,
    request: &RunRefineGateRevalidationRequest,
    target: &RegisteredRefineLeafTarget,
    report: &mut RefineGateExecutionReport,
) -> Result<RefineGateExecutionStatus, RefineGateExecutionError> {
    let mut first_failure_fingerprint = None::<String>;
    for attempt_index in 1..=request.retry_policy.max_invocation_retries {
        let fingerprint = leaf_fingerprint(&request.plan_root, &target.relative_path)?;
        if let Some(previous) = &first_failure_fingerprint {
            if previous != &fingerprint {
                return Err(RefineGateExecutionError::InvalidRegisteredTargets {
                    reason: format!(
                        "content fingerprint changed before retry for {}",
                        target.relative_path
                    ),
                });
            }
        }
        match runtime.run_leaf_review_gate(RunLeafReviewGateRequest {
            plan_id: request.plan_id.clone(),
            node_id: target.node_id.clone(),
            planner_mode: request.planner_mode.clone(),
            refine_revalidation_context: Some(render_refine_revalidation_context(
                "leaf",
                &target.relative_path,
                &target.node_id,
                target.parent_relative_path.as_deref(),
                &request.refine_context,
            )?),
        }) {
            Ok(response) if response.passed => {
                report.leaf_attempts.push(RefineGateAttempt {
                    gate_kind: RefineGateKind::Leaf,
                    target_node_id: target.node_id.clone(),
                    target_relative_path: target.relative_path.clone(),
                    attempt_index,
                    outcome: RefineGateAttemptOutcome::Passed {
                        gate_run_id: response.gate_run_id,
                        verdict: response.verdict,
                        summary: response.summary,
                    },
                    content_fingerprint: fingerprint,
                    invalidated_leaf_node_ids: Vec::new(),
                });
                return Ok(RefineGateExecutionStatus::Passed);
            }
            Ok(response) => {
                let status = status_for_review_issues(&response.issues);
                if status == RefineGateExecutionStatus::PausedForUserDecision {
                    report.paused_for_user_decision = response
                        .issues
                        .iter()
                        .find_map(|issue| issue.question_for_user.clone());
                    report.leaf_attempts.push(RefineGateAttempt {
                        gate_kind: RefineGateKind::Leaf,
                        target_node_id: target.node_id.clone(),
                        target_relative_path: target.relative_path.clone(),
                        attempt_index,
                        outcome: RefineGateAttemptOutcome::PauseForUserDecision {
                            summary: response.summary,
                            issues: response.issues,
                        },
                        content_fingerprint: fingerprint,
                        invalidated_leaf_node_ids: Vec::new(),
                    });
                } else {
                    report
                        .blocked_by_review_issues
                        .extend(response.issues.clone());
                    report.leaf_attempts.push(RefineGateAttempt {
                        gate_kind: RefineGateKind::Leaf,
                        target_node_id: target.node_id.clone(),
                        target_relative_path: target.relative_path.clone(),
                        attempt_index,
                        outcome: RefineGateAttemptOutcome::ReviewIssues {
                            verdict: response.verdict,
                            issues: response.issues,
                        },
                        content_fingerprint: fingerprint,
                        invalidated_leaf_node_ids: Vec::new(),
                    });
                }
                return Ok(status);
            }
            Err(source) => {
                first_failure_fingerprint.get_or_insert_with(|| fingerprint.clone());
                let error = source.to_string();
                report.leaf_attempts.push(RefineGateAttempt {
                    gate_kind: RefineGateKind::Leaf,
                    target_node_id: target.node_id.clone(),
                    target_relative_path: target.relative_path.clone(),
                    attempt_index,
                    outcome: RefineGateAttemptOutcome::InvocationFailure {
                        error: error.clone(),
                    },
                    content_fingerprint: fingerprint.clone(),
                    invalidated_leaf_node_ids: Vec::new(),
                });
                if attempt_index == request.retry_policy.max_invocation_retries {
                    report
                        .exhausted_invocation_retries
                        .push(RefineGateInvocationFailure {
                            gate_kind: RefineGateKind::Leaf,
                            target_node_id: target.node_id.clone(),
                            attempt_index,
                            error,
                            content_fingerprint: fingerprint,
                        });
                    return Ok(RefineGateExecutionStatus::ExhaustedInvocationRetries);
                }
            }
        }
    }
    Ok(RefineGateExecutionStatus::ExhaustedInvocationRetries)
}

fn run_frontier_target(
    runtime: &Runtime,
    request: &RunRefineGateRevalidationRequest,
    target: &RegisteredRefineFrontierTarget,
    report: &mut RefineGateExecutionReport,
) -> Result<RefineGateExecutionStatus, RefineGateExecutionError> {
    let mut first_failure_fingerprint = None::<String>;
    let target_parent_relative_path = runtime
        .inspect_node(InspectNodeRequest {
            plan_id: request.plan_id.clone(),
            node_id: Some(target.parent_node_id.clone()),
            relative_path: None,
        })
        .map_err(
            |source| RefineGateExecutionError::InvalidRegisteredTargets {
                reason: format!(
                    "failed to inspect frontier target parent `{}`: {source}",
                    target.parent_relative_path
                ),
            },
        )?
        .parent_relative_path;
    for attempt_index in 1..=request.retry_policy.max_invocation_retries {
        let fingerprint = frontier_fingerprint(runtime, request, target)?;
        if let Some(previous) = &first_failure_fingerprint {
            if previous != &fingerprint {
                return Err(RefineGateExecutionError::InvalidRegisteredTargets {
                    reason: format!(
                        "content fingerprint changed before retry for {}",
                        target.parent_relative_path
                    ),
                });
            }
        }
        match runtime.run_frontier_review_gate(RunFrontierReviewGateRequest {
            plan_id: request.plan_id.clone(),
            parent_node_id: target.parent_node_id.clone(),
            planner_mode: request.planner_mode.clone(),
            refine_revalidation_context: Some(render_refine_revalidation_context(
                "frontier",
                &target.parent_relative_path,
                &target.parent_node_id,
                target_parent_relative_path.as_deref(),
                &request.refine_context,
            )?),
        }) {
            Ok(response) if response.passed => {
                validate_frontier_response_contract(&response)?;
                report.frontier_attempts.push(RefineGateAttempt {
                    gate_kind: RefineGateKind::Frontier,
                    target_node_id: target.parent_node_id.clone(),
                    target_relative_path: target.parent_relative_path.clone(),
                    attempt_index,
                    outcome: RefineGateAttemptOutcome::Passed {
                        gate_run_id: response.gate_run_id,
                        verdict: response.verdict,
                        summary: response.summary,
                    },
                    content_fingerprint: fingerprint,
                    invalidated_leaf_node_ids: Vec::new(),
                });
                return Ok(RefineGateExecutionStatus::Passed);
            }
            Ok(response) => {
                // valid non-passed Runtime::run_frontier_review_gate response; collect
                // stable-deduplicated union of all valid non-passed frontier response invalidations.
                // must not present invalidated leaf ids as current approvals.
                for invalidated in &response.invalidated_leaf_node_ids {
                    if !report.invalidated_leaf_node_ids.contains(invalidated) {
                        report.invalidated_leaf_node_ids.push(invalidated.clone());
                    }
                }
                let status = status_for_review_issues(&response.issues);
                if status == RefineGateExecutionStatus::PausedForUserDecision {
                    report.paused_for_user_decision = response
                        .issues
                        .iter()
                        .find_map(|issue| issue.question_for_user.clone());
                    report.frontier_attempts.push(RefineGateAttempt {
                        gate_kind: RefineGateKind::Frontier,
                        target_node_id: target.parent_node_id.clone(),
                        target_relative_path: target.parent_relative_path.clone(),
                        attempt_index,
                        outcome: RefineGateAttemptOutcome::PauseForUserDecision {
                            summary: response.summary,
                            issues: response.issues,
                        },
                        content_fingerprint: fingerprint,
                        invalidated_leaf_node_ids: response.invalidated_leaf_node_ids,
                    });
                } else {
                    report
                        .blocked_by_review_issues
                        .extend(response.issues.clone());
                    report.frontier_attempts.push(RefineGateAttempt {
                        gate_kind: RefineGateKind::Frontier,
                        target_node_id: target.parent_node_id.clone(),
                        target_relative_path: target.parent_relative_path.clone(),
                        attempt_index,
                        outcome: RefineGateAttemptOutcome::ReviewIssues {
                            verdict: response.verdict,
                            issues: response.issues,
                        },
                        content_fingerprint: fingerprint,
                        invalidated_leaf_node_ids: response.invalidated_leaf_node_ids,
                    });
                }
                return Ok(status);
            }
            Err(source) => {
                first_failure_fingerprint.get_or_insert_with(|| fingerprint.clone());
                let error = source.to_string();
                report.frontier_attempts.push(RefineGateAttempt {
                    gate_kind: RefineGateKind::Frontier,
                    target_node_id: target.parent_node_id.clone(),
                    target_relative_path: target.parent_relative_path.clone(),
                    attempt_index,
                    outcome: RefineGateAttemptOutcome::InvocationFailure {
                        error: error.clone(),
                    },
                    content_fingerprint: fingerprint.clone(),
                    invalidated_leaf_node_ids: Vec::new(),
                });
                if attempt_index == request.retry_policy.max_invocation_retries {
                    report
                        .exhausted_invocation_retries
                        .push(RefineGateInvocationFailure {
                            gate_kind: RefineGateKind::Frontier,
                            target_node_id: target.parent_node_id.clone(),
                            attempt_index,
                            error,
                            content_fingerprint: fingerprint,
                        });
                    return Ok(RefineGateExecutionStatus::ExhaustedInvocationRetries);
                }
            }
        }
    }
    Ok(RefineGateExecutionStatus::ExhaustedInvocationRetries)
}

fn status_for_review_issues(issues: &[ReviewIssue]) -> RefineGateExecutionStatus {
    if issues.iter().any(|issue| {
        issue
            .question_for_user
            .as_deref()
            .is_some_and(|q| !q.trim().is_empty())
    }) {
        RefineGateExecutionStatus::PausedForUserDecision
    } else {
        RefineGateExecutionStatus::BlockedByReviewIssues
    }
}

fn render_refine_revalidation_context(
    gate_kind: &str,
    target_relative_path: &str,
    target_node_id: &str,
    target_parent_relative_path: Option<&str>,
    refine_context: &RefineGateRevalidationContext,
) -> Result<String, RefineGateExecutionError> {
    let mut rendered = String::new();
    rendered.push_str("## Target\n");
    rendered.push_str(&format!("- Gate Kind: {gate_kind}\n"));
    rendered.push_str(&format!("- Target Relative Path: {target_relative_path}\n"));
    rendered.push_str(&format!("- Target Node ID: {target_node_id}\n"));
    if let Some(parent) = target_parent_relative_path {
        rendered.push_str(&format!("- Target Parent Relative Path: {parent}\n"));
    }

    append_json_section(
        &mut rendered,
        "Processed Comment Blocks",
        &refine_context.processed_comment_blocks,
    )?;
    append_json_section(
        &mut rendered,
        "Stale Handoff",
        &refine_context.stale_result_handoff,
    )?;

    if let Some(rewrite_result) = &refine_context.rewrite_result {
        append_json_section(
            &mut rendered,
            "Changed Files",
            &rewrite_result.changed_files,
        )?;
        append_json_section(
            &mut rendered,
            "Changed Child Links",
            &rewrite_result.structural_changes,
        )?;
        append_json_section(
            &mut rendered,
            "Context Invalidations",
            &rewrite_result.context_invalidations,
        )?;
        append_json_section(
            &mut rendered,
            "Expected Gate Targets",
            &rewrite_result.expected_gate_targets,
        )?;
    } else {
        rendered.push_str("\n## Changed Files\nNone\n");
        rendered.push_str("\n## Changed Child Links\nNone\n");
        rendered.push_str("\n## Context Invalidations\nNone\n");
        rendered.push_str("\n## Expected Gate Targets\nNone\n");
    }

    Ok(rendered)
}

fn append_json_section<T: Serialize>(
    rendered: &mut String,
    title: &str,
    value: &T,
) -> Result<(), RefineGateExecutionError> {
    let json = serde_json::to_string_pretty(value).map_err(|source| {
        RefineGateExecutionError::InvalidRegisteredTargets {
            reason: source.to_string(),
        }
    })?;
    rendered.push_str(&format!("\n## {title}\n```json\n{json}\n```\n"));
    Ok(())
}

fn validate_frontier_response_contract(
    response: &RunFrontierReviewGateResponse,
) -> Result<(), RefineGateExecutionError> {
    if response.passed && !response.invalidated_leaf_node_ids.is_empty() {
        return invalid("passed frontier target must not invalidate leaf approvals");
    }
    Ok(())
}

fn validate_registered_targets(
    runtime: &Runtime,
    request: &RunRefineGateRevalidationRequest,
) -> Result<(), RefineGateExecutionError> {
    if request.retry_policy.max_invocation_retries == 0 {
        return invalid("retry_policy.max_invocation_retries must be at least 1");
    }
    if !request.plan_root.is_dir() {
        return invalid("plan_root must exist and be a directory");
    }
    let mut leaf_ids = HashSet::new();
    let mut leaf_paths = HashSet::new();
    for target in &request.registered_targets.leaf_targets {
        if target.node_id.is_empty() || target.reasons.is_empty() {
            return invalid("leaf target node_id and reasons must be non-empty");
        }
        validate_target_path(&request.plan_root, &target.relative_path)?;
        if !leaf_ids.insert(target.node_id.clone())
            || !leaf_paths.insert(target.relative_path.clone())
        {
            return invalid("duplicate leaf target node_id or relative_path");
        }
        if !request.plan_root.join(&target.relative_path).is_file() {
            return invalid("leaf markdown file must exist under plan_root");
        }
        validate_leaf_target_node(runtime, request, target)?;
    }

    for parent in &request.registered_targets.parent_targets {
        if parent.node_id.is_empty() || parent.relative_path.is_empty() || parent.reasons.is_empty()
        {
            return invalid("parent_targets are not dispatched by this API but must be canonical");
        }
        validate_target_path(&request.plan_root, &parent.relative_path)?;
    }

    let mut frontier_ids = HashSet::new();
    let mut frontier_paths = HashSet::new();
    for target in &request.registered_targets.frontier_targets {
        if target.parent_node_id.is_empty() || target.reasons.is_empty() {
            return invalid("frontier target parent_node_id and reasons must be non-empty");
        }
        validate_target_path(&request.plan_root, &target.parent_relative_path)?;
        if !frontier_ids.insert(target.parent_node_id.clone())
            || !frontier_paths.insert(target.parent_relative_path.clone())
        {
            return invalid("duplicate frontier target parent_node_id or parent_relative_path");
        }
        if !request
            .plan_root
            .join(&target.parent_relative_path)
            .is_file()
        {
            return invalid("frontier parent markdown file must exist under plan_root");
        }
        validate_frontier_target_node(runtime, request, target)?;
        let linked_child_paths =
            linked_child_paths_for_parent(&request.plan_root, &target.parent_relative_path)?;
        let children = runtime
            .list_children(ListChildrenRequest {
                plan_id: request.plan_id.clone(),
                parent_node_id: Some(target.parent_node_id.clone()),
                parent_relative_path: None,
            })
            .map_err(
                |source| RefineGateExecutionError::InvalidRegisteredTargets {
                    reason: source.to_string(),
                },
            )?;
        let visible_children: HashSet<_> = children
            .children
            .into_iter()
            .map(|child| child.relative_path)
            .collect();
        for child in &target.changed_child_relative_paths {
            validate_target_path(&request.plan_root, child)?;
            if linked_child_paths.contains(child) {
                if !visible_children.contains(child) {
                    return invalid("frontier linked child path must be runtime-visible");
                }
            } else if visible_children.contains(child) {
                return invalid("frontier removed child path must not be runtime-visible");
            } else if runtime
                .inspect_node(InspectNodeRequest {
                    plan_id: request.plan_id.clone(),
                    node_id: None,
                    relative_path: Some(child.clone()),
                })
                .is_err()
            {
                return invalid(
                    "frontier changed child path must be linked, runtime-visible, or tracked",
                );
            }
        }
    }
    Ok(())
}

fn validate_leaf_target_node(
    runtime: &Runtime,
    request: &RunRefineGateRevalidationRequest,
    target: &RegisteredRefineLeafTarget,
) -> Result<(), RefineGateExecutionError> {
    let node = runtime
        .inspect_node(InspectNodeRequest {
            plan_id: request.plan_id.clone(),
            node_id: Some(target.node_id.clone()),
            relative_path: None,
        })
        .map_err(
            |source| RefineGateExecutionError::InvalidRegisteredTargets {
                reason: source.to_string(),
            },
        )?;
    if node.node_kind != NodeKind::Leaf {
        return invalid("leaf target node_id must reference a leaf node");
    }
    if node.relative_path != target.relative_path {
        return invalid("leaf target node_id must match relative_path");
    }
    if node.parent_relative_path != target.parent_relative_path {
        return invalid("leaf target parent_relative_path must match runtime parent");
    }
    Ok(())
}

fn validate_frontier_target_node(
    runtime: &Runtime,
    request: &RunRefineGateRevalidationRequest,
    target: &RegisteredRefineFrontierTarget,
) -> Result<(), RefineGateExecutionError> {
    let node = runtime
        .inspect_node(InspectNodeRequest {
            plan_id: request.plan_id.clone(),
            node_id: Some(target.parent_node_id.clone()),
            relative_path: None,
        })
        .map_err(
            |source| RefineGateExecutionError::InvalidRegisteredTargets {
                reason: source.to_string(),
            },
        )?;
    if node.node_kind != NodeKind::Parent {
        return invalid("frontier target parent_node_id must reference a parent node");
    }
    if node.relative_path != target.parent_relative_path {
        return invalid("frontier target parent_node_id must match parent_relative_path");
    }
    Ok(())
}

fn linked_child_paths_for_parent(
    plan_root: &Path,
    parent_relative_path: &str,
) -> Result<HashSet<String>, RefineGateExecutionError> {
    let markdown = fs::read_to_string(plan_root.join(parent_relative_path)).map_err(|source| {
        RefineGateExecutionError::InvalidRegisteredTargets {
            reason: source.to_string(),
        }
    })?;
    crate::runtime::child_links::parse_child_node_link_paths(parent_relative_path, &markdown)
        .map_err(
            |source| RefineGateExecutionError::InvalidRegisteredTargets {
                reason: source.to_string(),
            },
        )?
        .into_iter()
        .map(|path| {
            validate_target_path(plan_root, &path)?;
            Ok(path)
        })
        .collect()
}

fn leaf_fingerprint(
    plan_root: &Path,
    relative_path: &str,
) -> Result<String, RefineGateExecutionError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("leaf\0{relative_path}\0").as_bytes());
    bytes.extend(read_target_bytes(plan_root, relative_path)?);
    Ok(sha256_hex(&bytes))
}

fn frontier_fingerprint(
    runtime: &Runtime,
    request: &RunRefineGateRevalidationRequest,
    target: &RegisteredRefineFrontierTarget,
) -> Result<String, RefineGateExecutionError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("frontier\0{}\0", target.parent_relative_path).as_bytes());
    bytes.extend(read_target_bytes(
        &request.plan_root,
        &target.parent_relative_path,
    )?);
    let children = runtime
        .list_children(ListChildrenRequest {
            plan_id: request.plan_id.clone(),
            parent_node_id: Some(target.parent_node_id.clone()),
            parent_relative_path: None,
        })
        .map_err(
            |source| RefineGateExecutionError::InvalidRegisteredTargets {
                reason: source.to_string(),
            },
        )?;
    for child in children.children {
        let Some(child_bytes) =
            read_existing_target_bytes(&request.plan_root, &child.relative_path)?
        else {
            continue;
        };
        bytes.extend_from_slice(format!("child\0{}\0", child.relative_path).as_bytes());
        bytes.extend(child_bytes);
    }
    Ok(sha256_hex(&bytes))
}

fn read_existing_target_bytes(
    plan_root: &Path,
    relative_path: &str,
) -> Result<Option<Vec<u8>>, RefineGateExecutionError> {
    validate_target_path(plan_root, relative_path)?;
    fs::read(plan_root.join(relative_path))
        .map(Some)
        .or_else(|source| {
            if source.kind() == ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(RefineGateExecutionError::InvalidRegisteredTargets {
                    reason: source.to_string(),
                })
            }
        })
}

fn read_target_bytes(
    plan_root: &Path,
    relative_path: &str,
) -> Result<Vec<u8>, RefineGateExecutionError> {
    validate_target_path(plan_root, relative_path)?;
    fs::read(plan_root.join(relative_path)).map_err(|source| {
        RefineGateExecutionError::InvalidRegisteredTargets {
            reason: source.to_string(),
        }
    })
}

fn validate_target_path(
    plan_root: &Path,
    relative_path: &str,
) -> Result<(), RefineGateExecutionError> {
    let path = Path::new(relative_path);
    if relative_path.is_empty()
        || path.is_absolute()
        || path.extension().and_then(|ext| ext.to_str()) != Some("md")
    {
        return invalid("target path must be a canonical markdown relative path");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return invalid("target path must stay inside plan_root");
        }
    }
    let joined = plan_root.join(relative_path);
    if !joined.starts_with(plan_root) {
        return invalid("plan_root.join target must remain inside plan_root");
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn invalid<T>(reason: &str) -> Result<T, RefineGateExecutionError> {
    Err(RefineGateExecutionError::InvalidRegisteredTargets {
        reason: reason.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::refine::{RefineGateTargetReason, RegisteredRefineGateTargets};
    use crate::{EnsureNodeIdRequest, EnsurePlanRequest};
    use rusqlite::{Connection, params};

    #[test]
    fn frontier_fingerprint_skips_runtime_children_deleted_from_disk() {
        let workspace = temp_workspace();
        let runtime = Runtime::new(&workspace).expect("runtime should initialize");
        let plan = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: "deleted-child-refine".to_owned(),
                task_type: "coding-task".to_owned(),
                project_directory: workspace.clone(),
            })
            .expect("plan should be created");
        let plan_root = PathBuf::from(&plan.plan_root);
        fs::create_dir_all(plan_root.join("api")).unwrap();
        fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n").unwrap();
        fs::write(plan_root.join("api/old-child.md"), "# Old Child\n").unwrap();

        let parent = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "api/api.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("parent should be tracked");
        runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "api/old-child.md".to_owned(),
                parent_relative_path: Some("api/api.md".to_owned()),
            })
            .expect("child should be tracked");
        fs::remove_file(plan_root.join("api/old-child.md")).unwrap();

        let request = RunRefineGateRevalidationRequest {
            plan_id: plan.plan_id,
            plan_root,
            planner_mode: PlannerMode::Manual,
            registered_targets: RegisteredRefineGateTargets::default(),
            retry_policy: RefineGateRetryPolicy::default(),
            refine_context: Default::default(),
        };
        let target = RegisteredRefineFrontierTarget {
            parent_node_id: parent.node_id,
            parent_relative_path: "api/api.md".to_owned(),
            changed_child_relative_paths: vec!["api/old-child.md".to_owned()],
            reasons: vec![RefineGateTargetReason::ChangedChildSet],
        };

        let fingerprint = frontier_fingerprint(&runtime, &request, &target)
            .expect("deleted children should not abort frontier fingerprinting");
        assert_eq!(fingerprint.len(), 64);
    }

    #[test]
    fn validate_registered_frontier_allows_removed_children_to_be_absent() {
        let workspace = temp_workspace();
        let runtime = Runtime::new(&workspace).expect("runtime should initialize");
        let plan = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: "removed-child-refine".to_owned(),
                task_type: "coding-task".to_owned(),
                project_directory: workspace.clone(),
            })
            .expect("plan should be created");
        let plan_root = PathBuf::from(&plan.plan_root);
        fs::create_dir_all(plan_root.join("api")).unwrap();
        fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n").unwrap();
        fs::write(plan_root.join("api/old-child.md"), "# Old Child\n").unwrap();

        let parent = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "api/api.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("parent should be tracked");
        let child = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "api/old-child.md".to_owned(),
                parent_relative_path: Some("api/api.md".to_owned()),
            })
            .expect("child should be tracked");
        let connection = Connection::open(workspace.join(".loopy/loopy.db")).unwrap();
        connection
            .execute(
                "UPDATE GEN_PLAN__nodes SET parent_node_id = NULL WHERE plan_id = ?1 AND node_id = ?2",
                params![plan.plan_id, child.node_id],
            )
            .unwrap();

        validate_registered_targets(
            &runtime,
            &RunRefineGateRevalidationRequest {
                plan_id: plan.plan_id,
                plan_root,
                planner_mode: PlannerMode::Manual,
                registered_targets: RegisteredRefineGateTargets {
                    frontier_targets: vec![RegisteredRefineFrontierTarget {
                        parent_node_id: parent.node_id,
                        parent_relative_path: "api/api.md".to_owned(),
                        changed_child_relative_paths: vec!["api/old-child.md".to_owned()],
                        reasons: vec![RefineGateTargetReason::ChangedChildSet],
                    }],
                    ..Default::default()
                },
                retry_policy: RefineGateRetryPolicy::default(),
                refine_context: Default::default(),
            },
        )
        .expect("removed child paths should not have to remain runtime-visible");
    }

    #[test]
    fn validate_registered_frontier_rejects_unknown_changed_child_paths() {
        let workspace = temp_workspace();
        let runtime = Runtime::new(&workspace).expect("runtime should initialize");
        let plan = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: "unknown-child-refine".to_owned(),
                task_type: "coding-task".to_owned(),
                project_directory: workspace.clone(),
            })
            .expect("plan should be created");
        let plan_root = PathBuf::from(&plan.plan_root);
        fs::create_dir_all(plan_root.join("api")).unwrap();
        fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n").unwrap();

        let parent = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "api/api.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("parent should be tracked");

        let error = validate_registered_targets(
            &runtime,
            &RunRefineGateRevalidationRequest {
                plan_id: plan.plan_id,
                plan_root,
                planner_mode: PlannerMode::Manual,
                registered_targets: RegisteredRefineGateTargets {
                    frontier_targets: vec![RegisteredRefineFrontierTarget {
                        parent_node_id: parent.node_id,
                        parent_relative_path: "api/api.md".to_owned(),
                        changed_child_relative_paths: vec!["api/typo.md".to_owned()],
                        reasons: vec![RefineGateTargetReason::ChangedChildSet],
                    }],
                    ..Default::default()
                },
                retry_policy: RefineGateRetryPolicy::default(),
                refine_context: Default::default(),
            },
        )
        .expect_err("unknown changed child paths should fail preflight");

        assert!(
            format!("{error:?}").contains(
                "frontier changed child path must be linked, runtime-visible, or tracked"
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn validate_registered_leaf_rejects_node_id_path_mismatch() {
        let workspace = temp_workspace();
        let runtime = Runtime::new(&workspace).expect("runtime should initialize");
        let plan = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: "mismatched-leaf-refine".to_owned(),
                task_type: "coding-task".to_owned(),
                project_directory: workspace.clone(),
            })
            .expect("plan should be created");
        let plan_root = PathBuf::from(&plan.plan_root);
        fs::write(plan_root.join("first.md"), "# First\n").unwrap();
        fs::write(plan_root.join("second.md"), "# Second\n").unwrap();
        let first = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "first.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("first leaf should be tracked");
        runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "second.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("second leaf should be tracked");

        let error = validate_registered_targets(
            &runtime,
            &RunRefineGateRevalidationRequest {
                plan_id: plan.plan_id,
                plan_root,
                planner_mode: PlannerMode::Manual,
                registered_targets: RegisteredRefineGateTargets {
                    leaf_targets: vec![RegisteredRefineLeafTarget {
                        node_id: first.node_id,
                        relative_path: "second.md".to_owned(),
                        parent_relative_path: None,
                        reasons: vec![RefineGateTargetReason::TextChanged],
                    }],
                    ..Default::default()
                },
                retry_policy: RefineGateRetryPolicy::default(),
                refine_context: Default::default(),
            },
        )
        .expect_err("mismatched leaf node_id and relative_path should fail preflight");

        assert!(
            format!("{error:?}").contains("leaf target node_id must match relative_path"),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn validate_registered_frontier_rejects_node_id_path_mismatch() {
        let workspace = temp_workspace();
        let runtime = Runtime::new(&workspace).expect("runtime should initialize");
        let plan = runtime
            .ensure_plan(EnsurePlanRequest {
                plan_name: "mismatched-frontier-refine".to_owned(),
                task_type: "coding-task".to_owned(),
                project_directory: workspace.clone(),
            })
            .expect("plan should be created");
        let plan_root = PathBuf::from(&plan.plan_root);
        fs::create_dir_all(plan_root.join("api")).unwrap();
        fs::create_dir_all(plan_root.join("other")).unwrap();
        fs::write(plan_root.join("api/api.md"), "# API\n").unwrap();
        fs::write(plan_root.join("other/other.md"), "# Other\n").unwrap();
        let api_parent = runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "api/api.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("api parent should be tracked");
        runtime
            .ensure_node_id(EnsureNodeIdRequest {
                plan_id: plan.plan_id.clone(),
                relative_path: "other/other.md".to_owned(),
                parent_relative_path: None,
            })
            .expect("other parent should be tracked");

        let error = validate_registered_targets(
            &runtime,
            &RunRefineGateRevalidationRequest {
                plan_id: plan.plan_id,
                plan_root,
                planner_mode: PlannerMode::Manual,
                registered_targets: RegisteredRefineGateTargets {
                    frontier_targets: vec![RegisteredRefineFrontierTarget {
                        parent_node_id: api_parent.node_id,
                        parent_relative_path: "other/other.md".to_owned(),
                        changed_child_relative_paths: Vec::new(),
                        reasons: vec![RefineGateTargetReason::ParentContractChanged],
                    }],
                    ..Default::default()
                },
                retry_policy: RefineGateRetryPolicy::default(),
                refine_context: Default::default(),
            },
        )
        .expect_err("mismatched frontier parent_node_id and path should fail preflight");

        assert!(
            format!("{error:?}")
                .contains("frontier target parent_node_id must match parent_relative_path"),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn passed_frontier_response_with_invalidations_is_invalid() {
        let response = crate::RunFrontierReviewGateResponse {
            gate_run_id: "frontier-run-1".to_owned(),
            passed: true,
            verdict: "approved_frontier".to_owned(),
            summary: "approved but contradictory".to_owned(),
            reviewer_role_id: "codex_default".to_owned(),
            issues: Vec::new(),
            invalidated_leaf_node_ids: vec!["leaf-1".to_owned()],
        };

        let error = validate_frontier_response_contract(&response)
            .expect_err("passed frontier responses must not invalidate leaves");
        assert!(
            format!("{error:?}")
                .contains("passed frontier target must not invalidate leaf approvals"),
            "unexpected error: {error:?}"
        );
    }

    fn temp_workspace() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("loopy-refine-gate-exec-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
