use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use loopy_common_invocation::run_local_command;
use rusqlite::{Connection, params};
use serde::Deserialize;
use uuid::Uuid;

use super::{Runtime, query};
use crate::{
    NodeKind, ReviewIssue, RunFrontierReviewGateRequest, RunFrontierReviewGateResponse,
    RunLeafReviewGateRequest, RunLeafReviewGateResponse,
    refine::{RefineStaleGateClassification, RefineStaleResultHandoff, StaleGateTargetKind},
};

const LEAF_REVIEWER_ROLE_KIND: &str = "leaf_reviewer";
const FRONTIER_REVIEWER_ROLE_KIND: &str = "frontier_reviewer";
const STDIO_TRANSCRIPT_CAPTURE: &str = "stdio";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeafReviewerOutput {
    verdict: String,
    summary: String,
    issues: Vec<ReviewIssue>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontierReviewerOutput {
    verdict: String,
    summary: String,
    issues: Vec<ReviewIssue>,
    invalidated_leaf_node_ids: Vec<String>,
}

pub(crate) fn run_leaf_review_gate(
    runtime: &Runtime,
    connection: &Connection,
    request: RunLeafReviewGateRequest,
) -> Result<RunLeafReviewGateResponse> {
    let RunLeafReviewGateRequest {
        plan_id,
        node_id,
        planner_mode,
        refine_revalidation_context,
    } = request;
    let plan = query::load_gate_plan_context(connection, &plan_id)?;
    let node = query::load_node_record(connection, &plan_id, &node_id)?;
    query::require_leaf_node_record(&node)?;
    require_leaf_review_eligible_node(connection, &plan_id, &node_id)?;
    query::ensure_plan_markdown_file_exists(&plan.plan_root, &node.relative_path)?;

    let gate_run_id = Uuid::new_v4().to_string();
    let bundle = runtime.resolved_skill_bundle()?;
    let manifest = loopy_gen_plan_bundle::load_manifest(&bundle.bundle_root)?;
    let resolved_roles =
        loopy_gen_plan_bundle::resolve_gate_roles(&bundle.bundle_root, &manifest, &plan.task_type)?;
    let reviewer_role_id = resolved_roles.leaf_reviewer_role_id;
    let (_, reviewer_role_prompt, _, executor_profile) =
        loopy_gen_plan_bundle::load_task_type_role_definition(
            &bundle.bundle_root,
            &manifest,
            &plan.task_type,
            LEAF_REVIEWER_ROLE_KIND,
            &reviewer_role_id,
        )?;

    let domain_contract = loopy_gen_plan_bundle::load_domain_contract_prompt(&bundle.bundle_root)?;
    let runtime_prompt = loopy_gen_plan_bundle::load_leaf_runtime_prompt(&bundle.bundle_root)?;
    let parent_expansion_snapshot = render_leaf_parent_expansion_snapshot(
        connection,
        &plan.plan_root,
        &plan_id,
        node.parent_node_id.as_deref(),
    )?;
    let prompt = render_template(
        &format!("{domain_contract}\n\n{reviewer_role_prompt}\n\n{runtime_prompt}"),
        &[
            ("planner_mode", planner_mode.as_str().to_owned()),
            ("plan_id", plan.plan_id.clone()),
            ("node_id", node.node_id.clone()),
            ("plan_root_path", plan.plan_root.display().to_string()),
            (
                "project_directory",
                plan.project_directory.display().to_string(),
            ),
            (
                "leaf_node_markdown",
                read_plan_markdown(&plan.plan_root, &node.relative_path)?,
            ),
            ("parent_expansion_snapshot", parent_expansion_snapshot),
            (
                "refine_revalidation_context",
                render_optional_refine_context(refine_revalidation_context.as_deref()),
            ),
        ],
    );
    let output = dispatch_reviewer(
        runtime.workspace_root(),
        &bundle.bundle_bin,
        &plan.project_directory,
        &executor_profile,
        &gate_run_id,
        &prompt,
    )?;
    let output: LeafReviewerOutput =
        serde_json::from_str(&output).context("failed to parse leaf reviewer JSON result")?;

    let passed = match output.verdict.as_str() {
        "approved_as_leaf" => true,
        "revise_leaf" | "must_expand" | "pause_for_user_decision" => false,
        other => bail!("unsupported leaf reviewer verdict `{other}`"),
    };
    validate_review_issues(&output.verdict, &output.issues, passed)?;
    let response = RunLeafReviewGateResponse {
        gate_run_id: gate_run_id.clone(),
        passed,
        verdict: output.verdict,
        summary: output.summary,
        reviewer_role_id: reviewer_role_id.clone(),
        issues: output.issues,
    };

    connection
        .execute(
            "INSERT INTO GEN_PLAN__leaf_gate_runs (
                leaf_gate_run_id,
                plan_id,
                node_id,
                planner_mode,
                reviewer_role_id,
                passed,
                verdict,
                summary,
                issues_json,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &gate_run_id,
                &plan_id,
                &node_id,
                planner_mode.as_str(),
                &response.reviewer_role_id,
                response.passed,
                &response.verdict,
                &response.summary,
                serde_json::to_string(&response.issues)
                    .context("failed to serialize leaf gate issues")?,
                current_timestamp()?,
            ],
        )
        .context("failed to persist leaf gate run")?;

    Ok(response)
}

pub(crate) fn run_frontier_review_gate(
    runtime: &Runtime,
    connection: &Connection,
    request: RunFrontierReviewGateRequest,
) -> Result<RunFrontierReviewGateResponse> {
    let RunFrontierReviewGateRequest {
        plan_id,
        parent_node_id,
        planner_mode,
        refine_revalidation_context,
        refine_invalidatable_leaf_node_ids,
    } = request;
    let plan = query::load_gate_plan_context(connection, &plan_id)?;
    let parent_node = query::load_node_record(connection, &plan_id, &parent_node_id)?;
    query::require_parent_node_record(&parent_node)?;
    query::ensure_plan_markdown_file_exists(&plan.plan_root, &parent_node.relative_path)?;
    query::ensure_parent_child_runtime_coherence(connection, &plan_id, &parent_node)?;
    let gate_run_id = Uuid::new_v4().to_string();

    let bundle = runtime.resolved_skill_bundle()?;
    let manifest = loopy_gen_plan_bundle::load_manifest(&bundle.bundle_root)?;
    let resolved_roles =
        loopy_gen_plan_bundle::resolve_gate_roles(&bundle.bundle_root, &manifest, &plan.task_type)?;
    let reviewer_role_id = resolved_roles.frontier_reviewer_role_id;
    let (_, reviewer_role_prompt, _, executor_profile) =
        loopy_gen_plan_bundle::load_task_type_role_definition(
            &bundle.bundle_root,
            &manifest,
            &plan.task_type,
            FRONTIER_REVIEWER_ROLE_KIND,
            &reviewer_role_id,
        )?;

    let domain_contract = loopy_gen_plan_bundle::load_domain_contract_prompt(&bundle.bundle_root)?;
    let runtime_prompt = loopy_gen_plan_bundle::load_frontier_runtime_prompt(&bundle.bundle_root)?;
    let frontier_expansion_snapshot =
        render_frontier_expansion_snapshot(connection, &plan.plan_root, &plan_id, &parent_node_id)?;
    let prompt = render_template(
        &format!("{domain_contract}\n\n{reviewer_role_prompt}\n\n{runtime_prompt}"),
        &[
            ("planner_mode", planner_mode.as_str().to_owned()),
            ("plan_id", plan.plan_id.clone()),
            ("parent_node_id", parent_node.node_id.clone()),
            ("plan_root_path", plan.plan_root.display().to_string()),
            (
                "project_directory",
                plan.project_directory.display().to_string(),
            ),
            (
                "parent_node_markdown",
                read_plan_markdown(&plan.plan_root, &parent_node.relative_path)?,
            ),
            ("frontier_expansion_snapshot", frontier_expansion_snapshot),
            (
                "passed_leaf_review_summaries",
                render_passed_leaf_review_summaries(connection, &plan_id, &parent_node_id)?,
            ),
            (
                "refine_revalidation_context",
                render_optional_refine_context(refine_revalidation_context.as_deref()),
            ),
        ],
    );
    let output = dispatch_reviewer(
        runtime.workspace_root(),
        &bundle.bundle_bin,
        &plan.project_directory,
        &executor_profile,
        &gate_run_id,
        &prompt,
    )?;
    let output: FrontierReviewerOutput =
        serde_json::from_str(&output).context("failed to parse frontier reviewer JSON result")?;

    let passed = match output.verdict.as_str() {
        "approved_frontier" => true,
        "revise_frontier" | "reopen_parent_scope" | "pause_for_user_decision" => false,
        other => bail!("unsupported frontier reviewer verdict `{other}`"),
    };
    validate_review_issues(&output.verdict, &output.issues, passed)?;
    let valid_invalidations = match (
        has_refine_revalidation_context(&refine_revalidation_context),
        refine_invalidatable_leaf_node_ids,
    ) {
        (true, Some(node_ids)) => validate_refine_invalidatable_leaf_node_ids(
            connection,
            &plan_id,
            select_refine_context_invalidatable_leaf_node_ids(
                connection,
                &plan_id,
                &parent_node_id,
                refine_revalidation_context.as_deref().unwrap_or_default(),
            )?,
            node_ids,
        )?,
        (true, None) => select_refine_context_invalidatable_leaf_node_ids(
            connection,
            &plan_id,
            &parent_node_id,
            refine_revalidation_context.as_deref().unwrap_or_default(),
        )?,
        _ => select_leaf_child_node_ids(connection, &plan_id, &parent_node_id)?,
    };
    for invalidated_node_id in &output.invalidated_leaf_node_ids {
        if !valid_invalidations
            .iter()
            .any(|candidate| candidate == invalidated_node_id)
        {
            bail!(
                "frontier reviewer invalidated unknown leaf child `{invalidated_node_id}` for parent `{parent_node_id}`"
            );
        }
    }
    if passed && !output.invalidated_leaf_node_ids.is_empty() {
        bail!("approved_frontier must not invalidate leaf approvals");
    }
    let response = RunFrontierReviewGateResponse {
        gate_run_id: gate_run_id.clone(),
        passed,
        verdict: output.verdict,
        summary: output.summary,
        reviewer_role_id: reviewer_role_id.clone(),
        issues: output.issues,
        invalidated_leaf_node_ids: output.invalidated_leaf_node_ids,
    };

    connection
        .execute(
            "INSERT INTO GEN_PLAN__frontier_gate_runs (
                frontier_gate_run_id,
                plan_id,
                parent_node_id,
                planner_mode,
                reviewer_role_id,
                passed,
                verdict,
                summary,
                issues_json,
                invalidated_leaf_node_ids_json,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &gate_run_id,
                &plan_id,
                &parent_node_id,
                planner_mode.as_str(),
                &response.reviewer_role_id,
                response.passed,
                &response.verdict,
                &response.summary,
                serde_json::to_string(&response.issues)
                    .context("failed to serialize frontier gate issues")?,
                serde_json::to_string(&response.invalidated_leaf_node_ids)
                    .context("failed to serialize frontier gate invalidations")?,
                current_timestamp()?,
            ],
        )
        .context("failed to persist frontier gate run")?;

    Ok(response)
}

fn dispatch_reviewer(
    workspace_root: &Path,
    bundle_bin: &Path,
    project_directory: &Path,
    executor_profile: &loopy_gen_plan_bundle::ExecutorProfile,
    gate_run_id: &str,
    prompt: &str,
) -> Result<String> {
    if executor_profile.kind != "local_command" {
        bail!(
            "unsupported gen-plan executor kind `{}`",
            executor_profile.kind
        );
    }
    if executor_profile.transcript_capture != STDIO_TRANSCRIPT_CAPTURE {
        bail!(
            "unsupported gen-plan transcript_capture `{}`",
            executor_profile.transcript_capture
        );
    }

    let gate_artifact_dir = workspace_root
        .join(".loopy")
        .join("gate-runs")
        .join(gate_run_id);
    fs::create_dir_all(&gate_artifact_dir).with_context(|| {
        format!(
            "failed to create gate artifact directory {}",
            gate_artifact_dir.display()
        )
    })?;
    let invocation_payload_path = gate_artifact_dir.join("prompt.md");
    let output_last_message_path = gate_artifact_dir.join("last-message.json");
    fs::write(&invocation_payload_path, prompt).with_context(|| {
        format!(
            "failed to write reviewer prompt {}",
            invocation_payload_path.display()
        )
    })?;

    let command = loopy_gen_plan_bundle::resolve_executor_command(
        executor_profile,
        bundle_bin,
        workspace_root,
        project_directory,
        &invocation_payload_path,
        &output_last_message_path,
    );
    let cwd = PathBuf::from(loopy_gen_plan_bundle::resolve_executor_cwd(
        &executor_profile.cwd,
        workspace_root,
        project_directory,
    ));
    let env_allow = executor_profile.env_allow.clone().unwrap_or_default();
    let output = run_local_command(
        &command,
        &cwd,
        Some(prompt),
        executor_profile.timeout_sec,
        "allowlist",
        &env_allow,
    )
    .with_context(|| format!("failed to dispatch reviewer for gate run `{gate_run_id}`"))?;
    if !output.status.success() {
        bail!(
            "reviewer command failed for gate run `{gate_run_id}`\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::read_to_string(&output_last_message_path).with_context(|| {
        format!(
            "failed to read reviewer last-message output {}",
            output_last_message_path.display()
        )
    })
}

fn render_leaf_parent_expansion_snapshot(
    connection: &Connection,
    plan_root: &Path,
    plan_id: &str,
    parent_node_id: Option<&str>,
) -> Result<String> {
    render_node_snapshot(
        plan_root,
        query::load_child_nodes(connection, plan_id, parent_node_id)?,
    )
}

fn render_frontier_expansion_snapshot(
    connection: &Connection,
    plan_root: &Path,
    plan_id: &str,
    parent_node_id: &str,
) -> Result<String> {
    render_node_snapshot(
        plan_root,
        query::load_child_nodes(connection, plan_id, Some(parent_node_id))?,
    )
}

fn render_node_snapshot(plan_root: &Path, nodes: Vec<query::NodeRecord>) -> Result<String> {
    if nodes.is_empty() {
        return Ok("_No nodes currently exist in this scope._".to_owned());
    }

    nodes
        .into_iter()
        .map(|node| {
            Ok(format!(
                "### Node `{}` (`{}`)\n```markdown\n{}\n```",
                node.node_id,
                node.relative_path,
                read_plan_markdown(plan_root, &node.relative_path)?
            ))
        })
        .collect::<Result<Vec<_>>>()
        .map(|sections| sections.join("\n\n"))
}

fn render_passed_leaf_review_summaries(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: &str,
) -> Result<String> {
    let child_nodes = query::load_child_nodes(connection, plan_id, Some(parent_node_id))?;
    let mut summaries = Vec::new();
    for child_node in child_nodes {
        if node_has_children(connection, plan_id, &child_node.node_id)? {
            continue;
        }
        if let Some(summary) =
            query::load_latest_passed_leaf_gate_summary(connection, plan_id, &child_node.node_id)?
        {
            summaries.push(format!(
                "- node_id `{}` (`{}`), gate_run_id `{}`, reviewer_role_id `{}`: {}",
                child_node.node_id,
                child_node.relative_path,
                summary.gate_run_id,
                summary.reviewer_role_id,
                summary.summary
            ));
        }
    }

    if summaries.is_empty() {
        Ok("- No passed leaf review summaries are currently available.".to_owned())
    } else {
        Ok(summaries.join("\n"))
    }
}

fn read_plan_markdown(plan_root: &Path, relative_path: &str) -> Result<String> {
    let full_path = plan_root.join(relative_path);
    fs::read_to_string(&full_path)
        .with_context(|| format!("failed to read plan markdown {}", full_path.display()))
}

fn render_optional_refine_context(context: Option<&str>) -> String {
    match context.map(str::trim).filter(|context| !context.is_empty()) {
        Some(context) => context.to_owned(),
        None => "No refine revalidation context supplied.".to_owned(),
    }
}

fn render_template(template: &str, replacements: &[(&str, String)]) -> String {
    let mut rendered = template.to_owned();
    for (key, value) in replacements {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    rendered
}

fn validate_review_issues(verdict: &str, issues: &[ReviewIssue], passed: bool) -> Result<()> {
    if passed {
        if !issues.is_empty() {
            bail!("approved reviewer results must not include issues");
        }
        return Ok(());
    }
    if issues.is_empty() {
        bail!("non-passing reviewer results must include at least one issue");
    }
    if issues.iter().any(|issue| {
        issue.target_node_id.is_none()
            && issue.target_parent_node_id.is_none()
            && issue
                .target_node_ids
                .as_ref()
                .is_none_or(|target_node_ids| target_node_ids.is_empty())
    }) {
        bail!("review issues must include an explicit target");
    }
    if verdict == "pause_for_user_decision"
        && issues.iter().any(|issue| {
            issue
                .question_for_user
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
                || issue
                    .decision_impact
                    .as_deref()
                    .map(str::trim)
                    .is_none_or(str::is_empty)
        })
    {
        bail!(
            "pause_for_user_decision issues must include non-empty question_for_user and decision_impact"
        );
    }
    Ok(())
}

fn require_leaf_review_eligible_node(
    connection: &Connection,
    plan_id: &str,
    node_id: &str,
) -> Result<()> {
    if node_has_children(connection, plan_id, node_id)? {
        return Err(anyhow!(
            "node_id `{node_id}` is not eligible for leaf review because it has child nodes"
        ));
    }

    Ok(())
}

fn node_has_children(connection: &Connection, plan_id: &str, node_id: &str) -> Result<bool> {
    let has_children = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM GEN_PLAN__nodes
                 WHERE plan_id = ?1 AND parent_node_id = ?2
             )",
            params![plan_id, node_id],
            |row| row.get::<_, i64>(0),
        )
        .context("failed to validate leaf review node eligibility")?;
    Ok(has_children != 0)
}

fn select_leaf_child_node_ids(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: &str,
) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare(
            "SELECT node_id
             FROM GEN_PLAN__nodes AS child
             WHERE child.plan_id = ?1
               AND child.parent_node_id = ?2
               AND NOT EXISTS (
                   SELECT 1
                   FROM GEN_PLAN__nodes AS grandchild
                   WHERE grandchild.plan_id = child.plan_id
                     AND grandchild.parent_node_id = child.node_id
               )
             ORDER BY child.relative_path, child.node_id",
        )
        .context("failed to prepare frontier leaf child lookup")?;

    statement
        .query_map(params![plan_id, parent_node_id], |row| row.get(0))
        .context("failed to query leaf child nodes for frontier gate")?
        .collect::<std::result::Result<Vec<String>, _>>()
        .context("failed to read leaf child nodes for frontier gate")
}

fn select_plan_leaf_node_ids(connection: &Connection, plan_id: &str) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare(
            "SELECT node_id
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1
               AND node_kind = 'leaf'
             ORDER BY relative_path, node_id",
        )
        .context("failed to prepare plan leaf lookup")?;

    statement
        .query_map(params![plan_id], |row| row.get(0))
        .context("failed to query plan leaf nodes for frontier gate")?
        .collect::<std::result::Result<Vec<String>, _>>()
        .context("failed to read plan leaf nodes for frontier gate")
}

fn select_refine_context_invalidatable_leaf_node_ids(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: &str,
    refine_context: &str,
) -> Result<Vec<String>> {
    let mut node_ids = select_descendant_leaf_node_ids(connection, plan_id, parent_node_id)?;
    let parent = query::load_node_record(connection, plan_id, parent_node_id)?;
    for node_id in stale_leaf_node_ids_from_refine_context(
        refine_context,
        parent_node_id,
        &parent.relative_path,
    )? {
        push_unique(&mut node_ids, node_id);
    }
    Ok(node_ids)
}

fn select_descendant_leaf_node_ids(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: &str,
) -> Result<Vec<String>> {
    let mut node_ids = Vec::new();
    collect_descendant_leaf_node_ids(connection, plan_id, parent_node_id, &mut node_ids)?;
    Ok(node_ids)
}

fn collect_descendant_leaf_node_ids(
    connection: &Connection,
    plan_id: &str,
    parent_node_id: &str,
    node_ids: &mut Vec<String>,
) -> Result<()> {
    for child in query::load_child_nodes(connection, plan_id, Some(parent_node_id))? {
        match child.node_kind {
            NodeKind::Leaf => push_unique(node_ids, child.node_id),
            NodeKind::Parent => {
                collect_descendant_leaf_node_ids(connection, plan_id, &child.node_id, node_ids)?
            }
        }
    }
    Ok(())
}

fn stale_leaf_node_ids_from_refine_context(
    refine_context: &str,
    parent_node_id: &str,
    parent_relative_path: &str,
) -> Result<Vec<String>> {
    let Some(json) = json_section(refine_context, "Stale Handoff") else {
        return Ok(Vec::new());
    };
    let handoffs: Vec<RefineStaleResultHandoff> = serde_json::from_str(json)
        .context("failed to parse Stale Handoff refine context section")?;
    Ok(handoffs
        .into_iter()
        .filter(|handoff| {
            handoff.target_kind == StaleGateTargetKind::Leaf
                && handoff.classification == RefineStaleGateClassification::Stale
                && !handoff.invalidation_reason.trim().is_empty()
                && (handoff.parent_node_id.as_deref() == Some(parent_node_id)
                    || handoff.parent_relative_path.as_deref() == Some(parent_relative_path))
        })
        .filter_map(|handoff| handoff.node_id)
        .collect())
}

fn json_section<'a>(refine_context: &'a str, title: &str) -> Option<&'a str> {
    let marker = format!("## {title}");
    let section = refine_context.split_once(&marker)?.1;
    let json_start = section.find("```json")?;
    let json = &section[json_start + "```json".len()..];
    let json = json.strip_prefix('\n').unwrap_or(json);
    let json_end = json.find("```")?;
    Some(json[..json_end].trim())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn validate_refine_invalidatable_leaf_node_ids(
    connection: &Connection,
    plan_id: &str,
    allowed_node_ids: Vec<String>,
    node_ids: Vec<String>,
) -> Result<Vec<String>> {
    let plan_leaf_node_ids = select_plan_leaf_node_ids(connection, plan_id)?;
    let mut valid = Vec::new();
    for node_id in node_ids {
        if !plan_leaf_node_ids
            .iter()
            .any(|candidate| candidate == &node_id)
        {
            bail!("refine invalidatable node_id `{node_id}` is not a tracked leaf in this plan");
        }
        if !allowed_node_ids
            .iter()
            .any(|candidate| candidate == &node_id)
        {
            bail!("refine invalidatable leaf `{node_id}` is outside the reviewed frontier scope");
        }
        if !valid.contains(&node_id) {
            valid.push(node_id);
        }
    }
    Ok(valid)
}

fn has_refine_revalidation_context(context: &Option<String>) -> bool {
    context
        .as_deref()
        .map(str::trim)
        .is_some_and(|context| !context.is_empty())
}

fn current_timestamp() -> Result<String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the unix epoch")?
        .as_nanos()
        .to_string())
}
