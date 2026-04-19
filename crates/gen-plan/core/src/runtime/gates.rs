use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::{
    ReviewIssue, RunFrontierReviewGateRequest, RunFrontierReviewGateResponse,
    RunLeafReviewGateRequest, RunLeafReviewGateResponse,
};

const MOCK_REVIEWER_ROLE_ID: &str = "mock";

pub(crate) fn run_leaf_review_gate(
    connection: &Connection,
    request: RunLeafReviewGateRequest,
) -> Result<RunLeafReviewGateResponse> {
    let RunLeafReviewGateRequest {
        plan_id,
        node_id,
        planner_mode,
    } = request;
    require_node(connection, &plan_id, &node_id, "node_id")?;

    let response = RunLeafReviewGateResponse {
        passed: false,
        verdict: "revise_leaf".to_owned(),
        reviewer_role_id: MOCK_REVIEWER_ROLE_ID.to_owned(),
        issues: vec![ReviewIssue {
            issue_kind: "mock_leaf_issue".to_owned(),
            target_node_id: Some(node_id.clone()),
            target_parent_node_id: None,
            target_node_ids: None,
            summary: "Mock leaf review requires a revision.".to_owned(),
            rationale: "Task 4 uses deterministic mock reviewer execution.".to_owned(),
            expected_revision: "Revise the leaf before continuing.".to_owned(),
            question_for_user: None,
            decision_impact: None,
        }],
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
                issues_json,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Uuid::new_v4().to_string(),
                &plan_id,
                &node_id,
                planner_mode.as_str(),
                &response.reviewer_role_id,
                response.passed,
                &response.verdict,
                serde_json::to_string(&response.issues)
                    .context("failed to serialize leaf gate issues")?,
                current_timestamp()?,
            ],
        )
        .context("failed to persist leaf gate run")?;

    Ok(response)
}

pub(crate) fn run_frontier_review_gate(
    connection: &Connection,
    request: RunFrontierReviewGateRequest,
) -> Result<RunFrontierReviewGateResponse> {
    let RunFrontierReviewGateRequest {
        plan_id,
        parent_node_id,
        planner_mode,
    } = request;
    require_node(connection, &plan_id, &parent_node_id, "parent_node_id")?;

    let invalidated_leaf_node_ids = vec!["node-leaf-1".to_owned()];
    let response = RunFrontierReviewGateResponse {
        passed: false,
        verdict: "revise_frontier".to_owned(),
        reviewer_role_id: MOCK_REVIEWER_ROLE_ID.to_owned(),
        issues: vec![ReviewIssue {
            issue_kind: "mock_frontier_issue".to_owned(),
            target_node_id: None,
            target_parent_node_id: Some(parent_node_id.clone()),
            target_node_ids: Some(invalidated_leaf_node_ids.clone()),
            summary: "Mock frontier review invalidated a leaf.".to_owned(),
            rationale: "Task 4 uses deterministic mock reviewer execution.".to_owned(),
            expected_revision: "Revise the frontier and regenerate the invalidated leaf."
                .to_owned(),
            question_for_user: None,
            decision_impact: None,
        }],
        invalidated_leaf_node_ids,
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
                issues_json,
                invalidated_leaf_node_ids_json,
                created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                Uuid::new_v4().to_string(),
                &plan_id,
                &parent_node_id,
                planner_mode.as_str(),
                &response.reviewer_role_id,
                response.passed,
                &response.verdict,
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

fn require_node(connection: &Connection, plan_id: &str, node_id: &str, label: &str) -> Result<()> {
    let exists = connection
        .query_row(
            "SELECT 1
             FROM GEN_PLAN__nodes
             WHERE plan_id = ?1 AND node_id = ?2",
            params![plan_id, node_id],
            |_row| Ok(()),
        )
        .optional()
        .with_context(|| format!("failed to validate {label}"))?;

    if exists.is_none() {
        return Err(anyhow!(
            "{label} `{node_id}` does not exist for plan `{plan_id}`"
        ));
    }

    Ok(())
}

fn current_timestamp() -> Result<String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the unix epoch")?
        .as_secs()
        .to_string())
}
