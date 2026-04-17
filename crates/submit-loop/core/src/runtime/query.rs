// Query logic owns read-side state loading and show-loop summaries; it does not append events.

use super::*;

pub(crate) fn show_loop(runtime: &Runtime, request: ShowLoopRequest) -> Result<ShowLoopSummary> {
    let ShowLoopRequest { loop_id } = request;
    let db_path = runtime.db_path()?;
    if !db_path.exists() {
        bail!("unknown loop_id {loop_id}");
    }
    let mut connection = runtime.open_read_only_connection()?;
    let transaction = connection.transaction()?;
    let loop_summary = load_show_loop_core(&transaction, &loop_id)?
        .ok_or_else(|| anyhow!("unknown loop_id {loop_id}"))?;
    let top_level_updated_at = load_show_loop_freshness_token(&transaction, &loop_summary.loop_id)?
        .unwrap_or_else(|| loop_summary.updated_at.clone());
    let bypass_sandbox = load_show_loop_bypass_sandbox(&transaction, &loop_summary.loop_id)?;
    let caller_finalize = if loop_summary.status == "open" {
        load_show_loop_caller_finalize(&transaction, &loop_summary.loop_id)?
    } else {
        None
    };
    let plan = load_show_loop_plan(&transaction, &loop_summary.loop_id)?;
    let worktree = load_show_loop_worktree(&transaction, &loop_summary.loop_id)?;
    let latest_invocation = load_show_loop_latest_invocation(&transaction, &loop_summary.loop_id)?;
    let latest_review = load_show_loop_latest_review(&transaction, &loop_summary.loop_id)?;
    let result = load_show_loop_result(&transaction, &loop_summary.loop_id)?;

    Ok(ShowLoopSummary {
        loop_id,
        status: loop_summary.status,
        phase: loop_summary.phase,
        updated_at: top_level_updated_at,
        bypass_sandbox,
        caller_finalize,
        plan,
        worktree,
        latest_invocation,
        latest_review,
        result,
    })
}

pub(crate) fn ensure_loop_projection_current(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<()> {
    let has_events: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM CORE__events WHERE loop_id = ?1)",
        [loop_id],
        |row| row.get(0),
    )?;
    if !has_events {
        return Ok(());
    }
    projection::rebuild_single_loop_projections(transaction, loop_id)?;
    let has_projection: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1)",
        [loop_id],
        |row| row.get(0),
    )?;
    if !has_projection {
        bail!("failed to rebuild loop projection for {loop_id}");
    }
    Ok(())
}

pub(crate) fn ensure_invocation_projection_current(
    transaction: &Transaction<'_>,
    invocation_id: &str,
) -> Result<()> {
    if let Some(loop_id) = find_loop_id_for_invocation(transaction, invocation_id)? {
        projection::rebuild_single_loop_projections(transaction, &loop_id)?;
        let has_projection: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM CORE__invocation_current WHERE invocation_id = ?1)",
            [invocation_id],
            |row| row.get(0),
        )?;
        if !has_projection {
            bail!("failed to rebuild invocation projection for {invocation_id}");
        }
    }
    Ok(())
}

fn find_loop_id_for_invocation(
    transaction: &Transaction<'_>,
    invocation_id: &str,
) -> Result<Option<String>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT loop_id, payload_json
        FROM CORE__events
        WHERE event_name = 'CORE__invocation_opened'
        ORDER BY event_id DESC
        "#,
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let loop_id: String = row.get(0)?;
        let payload_json: String = row.get(1)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        if required_str(&payload, "invocation_id")? == invocation_id {
            return Ok(Some(loop_id));
        }
    }
    transaction
        .query_row(
            "SELECT loop_id FROM CORE__invocation_current WHERE invocation_id = ?1",
            [invocation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn load_invocation_dispatch_state(
    transaction: &Transaction<'_>,
    invocation_id: &str,
) -> Result<InvocationDispatchState> {
    ensure_invocation_projection_current(transaction, invocation_id)?;
    transaction
        .query_row(
            r#"
            SELECT loop_id, invocation_context_ref, executor_config_ref
            FROM CORE__invocation_current
            WHERE invocation_id = ?1
            "#,
            [invocation_id],
            |row| {
                Ok(InvocationDispatchState {
                    loop_id: row.get(0)?,
                    invocation_context_ref: row.get(1)?,
                    executor_config_ref: row.get(2)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown invocation_id {invocation_id}"))
}

pub(crate) fn load_invocation_state(
    transaction: &Transaction<'_>,
    invocation_id: &str,
) -> Result<InvocationState> {
    ensure_invocation_projection_current(transaction, invocation_id)?;
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
    )> = transaction
        .query_row(
            r#"
            SELECT invocation.loop_id,
                   invocation.invocation_role,
                   invocation.stage,
                   invocation.status,
                   capability.token,
                   capability.token_state,
                   capability.accepted_api,
                   capability.accepted_submission_id,
                   invocation.invocation_context_ref,
                   invocation.allowed_terminal_apis_json,
                   invocation.review_round_id,
                   invocation.review_slot_id
            FROM CORE__capability_current capability
            JOIN CORE__invocation_current invocation
              ON invocation.invocation_id = capability.invocation_id
            WHERE capability.invocation_id = ?1
            "#,
            [invocation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .optional()?;
    let (
        loop_id,
        invocation_role,
        stage,
        status,
        token,
        token_state,
        accepted_api,
        accepted_submission_id,
        invocation_context_ref,
        allowed_terminal_apis_json,
        review_round_id,
        review_slot_id,
    ) = row.ok_or_else(|| anyhow!("unknown invocation_id {invocation_id}"))?;
    let allowed_terminal_apis = serde_json::from_str(&allowed_terminal_apis_json)
        .context("failed to decode allowed terminal APIs")?;
    Ok(InvocationState {
        loop_id,
        invocation_role,
        stage,
        status,
        token,
        token_state,
        accepted_api,
        accepted_submission_id,
        invocation_context_ref,
        allowed_terminal_apis,
        review_round_id,
        review_slot_id,
    })
}

pub(crate) fn load_latest_timeout_extension_request(
    transaction: &Transaction<'_>,
    invocation_id: &str,
) -> Result<Option<TimeoutExtensionRequestState>> {
    ensure_invocation_projection_current(transaction, invocation_id)?;
    transaction
        .query_row(
            r#"
            SELECT latest_request_content_ref,
                   requested_timeout_sec,
                   progress_summary,
                   rationale
            FROM SUBMIT_LOOP__timeout_extension_current
            WHERE invocation_id = ?1
            "#,
            [invocation_id],
            |row| {
                Ok(TimeoutExtensionRequestState {
                    request_content_ref: row.get(0)?,
                    requested_timeout_sec: row.get(1)?,
                    progress_summary: row.get(2)?,
                    rationale: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn load_review_round_state(
    transaction: &Transaction<'_>,
    loop_id: &str,
    review_round_id: &str,
) -> Result<ReviewRoundState> {
    ensure_loop_projection_current(transaction, loop_id)?;
    let row: Option<(String, String, String, String, String)> = transaction
        .query_row(
            r#"
            SELECT review_kind, target_type, target_ref, target_metadata_json, slot_state_json
            FROM SUBMIT_LOOP__review_current
            WHERE loop_id = ?1 AND review_round_id = ?2
            "#,
            params![loop_id, review_round_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (review_kind, target_type, target_ref, target_metadata_json, slot_state_json) =
        row.ok_or_else(|| anyhow!("unknown review round {review_round_id} for loop {loop_id}"))?;
    let target_metadata = serde_json::from_str(&target_metadata_json)
        .context("failed to decode review target metadata")?;
    let slot_state = serde_json::from_str(&slot_state_json)
        .context("failed to decode review-round slot state")?;
    Ok(ReviewRoundState {
        review_kind,
        target_type,
        target_ref,
        target_metadata,
        slot_state,
    })
}

pub(crate) fn load_checkpoint_state(
    transaction: &Transaction<'_>,
    loop_id: &str,
    checkpoint_id: &str,
) -> Result<CheckpointState> {
    transaction
        .query_row(
            r#"
            SELECT checkpoint_id, sequence_index, title, kind, deliverables_json, acceptance_json, execution_state, accepted_commit_sha, candidate_commit_sha
            FROM SUBMIT_LOOP__checkpoint_current
            WHERE loop_id = ?1 AND checkpoint_id = ?2 AND active = 1
            "#,
            params![loop_id, checkpoint_id],
            decode_checkpoint_state_row,
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown active checkpoint {} for loop {}", checkpoint_id, loop_id))
}

pub(crate) fn load_previous_checkpoint_state(
    transaction: &Transaction<'_>,
    loop_id: &str,
    sequence_index: i64,
) -> Result<Option<CheckpointState>> {
    transaction
        .query_row(
            r#"
            SELECT checkpoint_id, sequence_index, title, kind, deliverables_json, acceptance_json, execution_state, accepted_commit_sha, candidate_commit_sha
            FROM SUBMIT_LOOP__checkpoint_current
            WHERE loop_id = ?1 AND sequence_index = ?2 AND active = 1
            "#,
            params![loop_id, sequence_index - 1],
            decode_checkpoint_state_row,
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn load_resolved_role_selection(
    transaction: &Transaction<'_>,
    skill_root: &Path,
    content_ref: &str,
) -> Result<ResolvedRoleSelection> {
    let value = load_json_content(transaction, content_ref)?;
    roles::decode_persisted_resolved_role_selection(skill_root, value)
        .context("failed to decode resolved_role_selection")
}

pub(crate) fn load_worktree_lifecycle(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<String>> {
    transaction
        .query_row(
            "SELECT lifecycle FROM SUBMIT_LOOP__worktree_current WHERE loop_id = ?1",
            [loop_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn load_active_checkpoint_state(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Vec<CheckpointState>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT checkpoint_id, sequence_index, title, kind, deliverables_json, acceptance_json, execution_state, accepted_commit_sha, candidate_commit_sha
        FROM SUBMIT_LOOP__checkpoint_current
        WHERE loop_id = ?1 AND active = 1
        ORDER BY sequence_index ASC
        "#,
    )?;
    let rows = statement.query_map([loop_id], decode_checkpoint_state_row)?;
    rows.collect::<Result<Vec<CheckpointState>, _>>()
        .map_err(Into::into)
}

fn decode_checkpoint_state_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointState> {
    let deliverables_json: String = row.get(4)?;
    let acceptance_json: String = row.get(5)?;
    let deliverables = serde_json::from_str(&deliverables_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let acceptance = serde_json::from_str(&acceptance_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(CheckpointState {
        checkpoint_id: row.get(0)?,
        sequence_index: row.get(1)?,
        title: row.get(2)?,
        kind: row.get(3)?,
        deliverables,
        acceptance,
        execution_state: row.get(6)?,
        accepted_commit_sha: row.get(7)?,
        candidate_commit_sha: row.get(8)?,
    })
}

pub(crate) fn build_checkpoint_payload(checkpoint: &CheckpointState) -> Value {
    json!({
        "checkpoint_id": checkpoint.checkpoint_id.clone(),
        "sequence_index": checkpoint.sequence_index,
        "title": checkpoint.title.clone(),
        "kind": checkpoint.kind.clone(),
        "deliverables": checkpoint.deliverables.clone(),
        "acceptance": checkpoint.acceptance.clone(),
    })
}

pub(crate) fn build_worker_review_history_payload(
    transaction: &Transaction<'_>,
    loop_id: &str,
    loop_state: &LoopState,
    stage: &WorkerStage,
    checkpoint_id: Option<&str>,
) -> Result<Value> {
    let relevant_results =
        load_worker_review_history_entries(transaction, loop_id, stage, checkpoint_id)?;
    Ok(json!({
        "loop_phase": loop_state.phase,
        "loop_status": loop_state.status,
        "latest_submitted_plan_revision": load_latest_submitted_plan_revision(transaction, loop_id)?,
        "failure_summary": loop_state.failure_summary,
        "latest_result": relevant_results.first().cloned().unwrap_or(Value::Null),
        "previous_results": relevant_results.into_iter().skip(1).collect::<Vec<_>>(),
    }))
}

pub(crate) fn build_reviewer_review_history_payload(
    transaction: &Transaction<'_>,
    loop_id: &str,
    review_kind: &str,
    reviewer_role_id: &str,
) -> Result<Value> {
    let relevant_results =
        load_reviewer_history_entries(transaction, loop_id, review_kind, reviewer_role_id)?;
    Ok(json!({
        "reviewer_role_id": reviewer_role_id,
        "latest_result": relevant_results.first().cloned().unwrap_or(Value::Null),
        "previous_results": relevant_results.into_iter().skip(1).collect::<Vec<_>>(),
    }))
}

fn load_worker_review_history_entries(
    transaction: &Transaction<'_>,
    loop_id: &str,
    stage: &WorkerStage,
    checkpoint_id: Option<&str>,
) -> Result<Vec<Value>> {
    let event_name = match stage {
        WorkerStage::Planning => "SUBMIT_LOOP__checkpoint_review_round_recorded",
        WorkerStage::Artifact => "SUBMIT_LOOP__artifact_review_round_recorded",
    };
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1 AND event_name = ?2
        ORDER BY event_id DESC
        "#,
    )?;
    let mut rows = statement.query(params![loop_id, event_name])?;
    let mut finalized_payloads = Vec::new();
    while let Some(row) = rows.next()? {
        let payload_json: String = row.get(0)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        if let Some(checkpoint_id) = checkpoint_id {
            if payload.get("target_type").and_then(Value::as_str) != Some("checkpoint_id") {
                continue;
            }
            if payload.get("target_ref").and_then(Value::as_str) != Some(checkpoint_id) {
                continue;
            }
        }
        if payload.get("round_status").and_then(Value::as_str) == Some("pending") {
            continue;
        }
        finalized_payloads.push(payload);
        if finalized_payloads.len() == 5 {
            break;
        }
    }
    let Some(latest_payload) = finalized_payloads.first() else {
        return Ok(Vec::new());
    };
    if latest_payload.get("round_status").and_then(Value::as_str) != Some("rejected") {
        return Ok(Vec::new());
    }
    finalized_payloads
        .into_iter()
        .enumerate()
        .map(|(index, payload)| {
            build_worker_review_history_entry(transaction, &payload, index == 0)
        })
        .collect()
}

fn load_reviewer_history_entries(
    transaction: &Transaction<'_>,
    loop_id: &str,
    review_kind: &str,
    reviewer_role_id: &str,
) -> Result<Vec<Value>> {
    let event_name = match review_kind {
        "checkpoint" => "SUBMIT_LOOP__checkpoint_review_round_recorded",
        "artifact" => "SUBMIT_LOOP__artifact_review_round_recorded",
        other => bail!("unsupported review kind {other} for reviewer history"),
    };
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1
          AND event_name = ?2
        ORDER BY event_id DESC
        "#,
    )?;
    let mut rows = statement.query(params![loop_id, event_name])?;
    let mut relevant_entries = Vec::new();
    while let Some(row) = rows.next()? {
        let payload_json: String = row.get(0)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        if let Some(entry) = build_reviewer_history_entry(
            transaction,
            &payload,
            reviewer_role_id,
            relevant_entries.is_empty(),
        )? {
            relevant_entries.push(entry);
            if relevant_entries.len() == 5 {
                break;
            }
        }
    }
    Ok(relevant_entries)
}

fn build_worker_review_history_entry(
    transaction: &Transaction<'_>,
    payload: &Value,
    include_full_result: bool,
) -> Result<Value> {
    let slot_state: Vec<ReviewSlotState> = serde_json::from_value(
        payload
            .get("slot_state")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .context("failed to decode review slot state for worker review history")?;
    let mut summaries = Vec::new();
    let mut blocking_issues = Vec::new();
    let mut nonblocking_issues = Vec::new();
    for slot in slot_state {
        let Some(submission_content_ref) = slot.submission_content_ref else {
            continue;
        };
        let submission = load_json_content(transaction, &submission_content_ref)?;
        if let Some(summary) = load_submission_summary(&submission) {
            summaries.push(summary);
        }
        blocking_issues.extend(load_submission_issue_objects(
            &submission,
            "blocking_issues",
        )?);
        nonblocking_issues.extend(load_submission_issue_objects(
            &submission,
            "nonblocking_issues",
        )?);
    }
    let mut entry = json!({
        "review_round_id": required_str(payload, "review_round_id")?,
        "review_kind": required_str(payload, "review_kind")?,
        "round_status": required_str(payload, "round_status")?,
        "target_type": required_str(payload, "target_type")?,
        "target_ref": required_str(payload, "target_ref")?,
        "target_metadata": payload.get("target_metadata").cloned().unwrap_or_else(|| json!({})),
        "summary": aggregate_revision_guidance_text(summaries),
    });
    if let Some(object) = entry.as_object_mut() {
        if include_full_result {
            object.insert("blocking_issues".to_owned(), Value::Array(blocking_issues));
            object.insert(
                "nonblocking_issues".to_owned(),
                Value::Array(nonblocking_issues),
            );
        } else {
            object.insert(
                "blocking_issue_count".to_owned(),
                json!(blocking_issues.len()),
            );
            object.insert(
                "nonblocking_issue_count".to_owned(),
                json!(nonblocking_issues.len()),
            );
        }
    }
    Ok(entry)
}

fn build_reviewer_history_entry(
    transaction: &Transaction<'_>,
    payload: &Value,
    reviewer_role_id: &str,
    include_full_result: bool,
) -> Result<Option<Value>> {
    let slot_state: Vec<ReviewSlotState> = serde_json::from_value(
        payload
            .get("slot_state")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .context("failed to decode review slot state for reviewer review history")?;
    let Some(slot) = slot_state.into_iter().find(|slot| {
        slot.reviewer_role_id.as_deref() == Some(reviewer_role_id)
            && slot.submission_content_ref.is_some()
    }) else {
        return Ok(None);
    };
    let submission_content_ref = slot
        .submission_content_ref
        .ok_or_else(|| anyhow!("review slot missing submission_content_ref"))?;
    let submission = load_json_content(transaction, &submission_content_ref)?;
    let mut entry = json!({
        "review_round_id": required_str(payload, "review_round_id")?,
        "review_kind": required_str(payload, "review_kind")?,
        "round_status": required_str(payload, "round_status")?,
        "target_type": required_str(payload, "target_type")?,
        "target_ref": required_str(payload, "target_ref")?,
        "target_metadata": payload.get("target_metadata").cloned().unwrap_or_else(|| json!({})),
        "decision": slot.status,
        "summary": load_submission_summary(&submission),
    });
    if let Some(object) = entry.as_object_mut() {
        if include_full_result {
            object.insert(
                "blocking_issues".to_owned(),
                Value::Array(load_submission_issue_objects(
                    &submission,
                    "blocking_issues",
                )?),
            );
            object.insert(
                "nonblocking_issues".to_owned(),
                Value::Array(load_submission_issue_objects(
                    &submission,
                    "nonblocking_issues",
                )?),
            );
            object.insert(
                "improvement_opportunities".to_owned(),
                Value::Array(load_submission_improvement_opportunities(&submission)?),
            );
        }
    }
    Ok(Some(entry))
}

fn load_submission_summary(submission: &Value) -> Option<String> {
    submission
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned)
}

fn load_submission_issue_objects(submission: &Value, key: &str) -> Result<Vec<Value>> {
    Ok(submission
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn load_submission_improvement_opportunities(submission: &Value) -> Result<Vec<Value>> {
    Ok(submission
        .get("improvement_opportunities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn dedupe_improvement_objects(values: Vec<Value>) -> Result<Vec<Value>> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        let key = serde_json::to_string(&value)?;
        if seen.insert(key) {
            deduped.push(value);
        }
    }
    Ok(deduped)
}

fn maybe_collect_reviewer_improvement_summary(
    transaction: &Transaction<'_>,
    payload: &Value,
    slot: ReviewSlotState,
    seen_sources: &mut std::collections::HashSet<String>,
) -> Result<Option<CallerFacingImprovementSummary>> {
    let Some(reviewer_role_id) = slot.reviewer_role_id else {
        return Ok(None);
    };
    let review_kind = required_str(payload, "review_kind")?;
    let source_key = format!("reviewer:{review_kind}:{reviewer_role_id}");
    if !seen_sources.insert(source_key) {
        return Ok(None);
    }
    let Some(submission_content_ref) = slot.submission_content_ref else {
        return Ok(None);
    };
    let submission = load_json_content(transaction, &submission_content_ref)?;
    let improvement_opportunities =
        dedupe_improvement_objects(load_submission_improvement_opportunities(&submission)?)?;
    if improvement_opportunities.is_empty() {
        return Ok(None);
    }
    Ok(Some(CallerFacingImprovementSummary {
        source: CallerFacingImprovementSource {
            kind: "reviewer".to_owned(),
            stage: None,
            review_kind: Some(review_kind.to_owned()),
            role_id: Some(reviewer_role_id),
        },
        summary: load_submission_summary(&submission),
        target_type: Some(required_str(payload, "target_type")?.to_owned()),
        target_ref: Some(required_str(payload, "target_ref")?.to_owned()),
        improvement_opportunities,
    }))
}

fn load_worker_improvement_role_id(
    transaction: &Transaction<'_>,
    invocation_id: &str,
) -> Result<Option<String>> {
    let invocation_context_ref: Option<String> = transaction
        .query_row(
            "SELECT invocation_context_ref FROM CORE__invocation_current WHERE invocation_id = ?1",
            [invocation_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(invocation_context_ref) = invocation_context_ref else {
        return Ok(None);
    };
    let invocation_context = load_json_content(transaction, &invocation_context_ref)?;
    Ok(invocation_context
        .get("selected_role_id")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

fn maybe_collect_worker_improvement_summary(
    transaction: &Transaction<'_>,
    payload: &Value,
    stage: &str,
    seen_sources: &mut std::collections::HashSet<String>,
) -> Result<Option<CallerFacingImprovementSummary>> {
    let invocation_id = required_str(payload, "invocation_id")?;
    let role_id = load_worker_improvement_role_id(transaction, invocation_id)?;
    let source_key = format!(
        "worker:{stage}:{}",
        role_id.as_deref().unwrap_or("<unknown>")
    );
    if !seen_sources.insert(source_key) {
        return Ok(None);
    }
    let submission_content_ref = required_str(payload, "submission_content_ref")?;
    let submission = load_json_content(transaction, submission_content_ref)?;
    let improvement_opportunities =
        dedupe_improvement_objects(load_submission_improvement_opportunities(&submission)?)?;
    if improvement_opportunities.is_empty() {
        return Ok(None);
    }
    let (target_type, target_ref) = match stage {
        "planning" => (
            Some("plan_revision".to_owned()),
            payload
                .get("plan_revision")
                .and_then(Value::as_i64)
                .map(|revision| format!("plan-{revision}")),
        ),
        "artifact" => (
            Some("checkpoint_id".to_owned()),
            payload
                .get("checkpoint_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        _ => (None, None),
    };
    let summary = submission
        .get("notes")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            submission
                .get("change_summary")
                .and_then(|value| value.get("headline"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        });
    Ok(Some(CallerFacingImprovementSummary {
        source: CallerFacingImprovementSource {
            kind: "worker".to_owned(),
            stage: Some(stage.to_owned()),
            review_kind: None,
            role_id,
        },
        summary,
        target_type,
        target_ref,
        improvement_opportunities,
    }))
}

pub(crate) fn load_caller_facing_improvement_summaries(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Vec<CallerFacingImprovementSummary>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT event_name, payload_json
        FROM CORE__events
        WHERE loop_id = ?1
          AND event_name IN (
            'SUBMIT_LOOP__plan_submitted',
            'SUBMIT_LOOP__candidate_commit_submitted',
            'SUBMIT_LOOP__checkpoint_review_round_recorded',
            'SUBMIT_LOOP__artifact_review_round_recorded'
          )
        ORDER BY event_id DESC
        "#,
    )?;
    let mut rows = statement.query([loop_id])?;
    let mut seen_sources = std::collections::HashSet::new();
    let mut summaries = Vec::new();
    while let Some(row) = rows.next()? {
        let event_name: String = row.get(0)?;
        let payload_json: String = row.get(1)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        match event_name.as_str() {
            "SUBMIT_LOOP__plan_submitted" => {
                if let Some(summary) = maybe_collect_worker_improvement_summary(
                    transaction,
                    &payload,
                    "planning",
                    &mut seen_sources,
                )? {
                    summaries.push(summary);
                }
            }
            "SUBMIT_LOOP__candidate_commit_submitted" => {
                if let Some(summary) = maybe_collect_worker_improvement_summary(
                    transaction,
                    &payload,
                    "artifact",
                    &mut seen_sources,
                )? {
                    summaries.push(summary);
                }
            }
            "SUBMIT_LOOP__checkpoint_review_round_recorded"
            | "SUBMIT_LOOP__artifact_review_round_recorded" => {
                let slot_state: Vec<ReviewSlotState> = serde_json::from_value(
                    payload
                        .get("slot_state")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                )
                .context("failed to decode review slot state for caller-facing improvements")?;
                for slot in slot_state {
                    if let Some(summary) = maybe_collect_reviewer_improvement_summary(
                        transaction,
                        &payload,
                        slot,
                        &mut seen_sources,
                    )? {
                        summaries.push(summary);
                    }
                }
            }
            other => bail!("unsupported improvement aggregation event {other}"),
        }
    }
    Ok(summaries)
}

fn aggregate_revision_guidance_text(values: Vec<String>) -> Value {
    match values.len() {
        0 => Value::Null,
        1 => Value::String(values.into_iter().next().unwrap()),
        _ => Value::String(values.join("\n\n")),
    }
}

pub(crate) fn load_latest_submitted_plan_revision(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<i64>> {
    Ok(transaction.query_row(
        "SELECT latest_submitted_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        [loop_id],
        |row| row.get(0),
    )?)
}

pub(crate) fn load_current_executable_plan_revision(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<i64>> {
    Ok(transaction.query_row(
        "SELECT current_executable_plan_revision FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        [loop_id],
        |row| row.get(0),
    )?)
}

pub(crate) fn load_submitted_plan_revision(
    transaction: &Transaction<'_>,
    loop_id: &str,
    invocation_id: &str,
    submission_id: &str,
) -> Result<Option<i64>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'
        ORDER BY event_id ASC
        "#,
    )?;
    let mut rows = statement.query([loop_id])?;
    while let Some(row) = rows.next()? {
        let payload_json: String = row.get(0)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        if required_str(&payload, "invocation_id")? == invocation_id
            && required_str(&payload, "submission_id")? == submission_id
        {
            return Ok(Some(
                payload
                    .get("plan_revision")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| anyhow!("missing integer field plan_revision"))?,
            ));
        }
    }
    Ok(None)
}

pub(crate) fn load_existing_result_ref(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<String>> {
    transaction
        .query_row(
            "SELECT result_ref FROM CORE__result_current WHERE loop_id = ?1",
            [loop_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn parse_plan_revision_target(target_type: &str, target_ref: &str) -> Result<i64> {
    if target_type != "plan_revision" {
        bail!("unsupported checkpoint review target_type {target_type}");
    }
    if let Ok(value) = target_ref.parse::<i64>() {
        return Ok(value);
    }
    if let Some(suffix) = target_ref.strip_prefix("plan-") {
        return suffix
            .parse::<i64>()
            .with_context(|| format!("failed to parse plan revision from {target_ref}"));
    }
    bail!("failed to parse plan revision from {target_ref}")
}

pub(crate) fn load_plan_submission_for_revision(
    transaction: &Transaction<'_>,
    loop_id: &str,
    plan_revision: i64,
) -> Result<Value> {
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__plan_submitted'
        ORDER BY event_id DESC
        "#,
    )?;
    let mut rows = statement.query([loop_id])?;
    while let Some(row) = rows.next()? {
        let payload_json: String = row.get(0)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        if payload.get("plan_revision").and_then(Value::as_i64) == Some(plan_revision) {
            return Ok(payload);
        }
    }
    bail!("missing submitted plan revision {plan_revision} for loop {loop_id}")
}

pub(crate) fn snapshot_checkpoint_review_target(
    transaction: &Transaction<'_>,
    loop_id: &str,
    target_type: &str,
    target_ref: &str,
) -> Result<Value> {
    let plan_revision = parse_plan_revision_target(target_type, target_ref)?;
    if let Some(latest_submitted_plan_revision) =
        load_latest_submitted_plan_revision(transaction, loop_id)?
    {
        if latest_submitted_plan_revision != plan_revision {
            bail!(
                "checkpoint review target {target_ref} is stale; latest submitted plan revision is plan-{latest_submitted_plan_revision}"
            );
        }
    }
    if load_current_executable_plan_revision(transaction, loop_id)? == Some(plan_revision) {
        bail!("checkpoint review target {target_ref} is already executable");
    }
    let submission = load_plan_submission_for_revision(transaction, loop_id, plan_revision)?;
    let checkpoints = submission.get("checkpoints").cloned().ok_or_else(|| {
        anyhow!("missing checkpoints for submitted plan revision {plan_revision}")
    })?;
    let mut target_metadata = json!({
        "plan_revision": plan_revision,
        "checkpoints": checkpoints,
    });
    if let Some(notes) = submission.get("notes").cloned() {
        if let Some(object) = target_metadata.as_object_mut() {
            object.insert("notes".to_owned(), notes);
        }
    }
    Ok(target_metadata)
}

pub(crate) fn snapshot_artifact_review_target(
    transaction: &Transaction<'_>,
    loop_id: &str,
    target_type: &str,
    target_ref: &str,
) -> Result<Value> {
    if target_type != "checkpoint_id" {
        bail!("unsupported artifact review target_type {target_type}");
    }
    let checkpoint = load_checkpoint_state(transaction, loop_id, target_ref)?;
    let candidate_commit_sha = checkpoint
        .candidate_commit_sha
        .clone()
        .ok_or_else(|| anyhow!("checkpoint {} has no candidate commit", target_ref))?;
    Ok(json!({
        "checkpoint_id": checkpoint.checkpoint_id.clone(),
        "sequence_index": checkpoint.sequence_index,
        "title": checkpoint.title.clone(),
        "kind": checkpoint.kind.clone(),
        "deliverables": checkpoint.deliverables.clone(),
        "acceptance": checkpoint.acceptance.clone(),
        "candidate_commit_sha": candidate_commit_sha,
    }))
}

pub(crate) fn build_review_target_payload(review_round_state: &ReviewRoundState) -> Value {
    let mut payload = json!({
        "type": review_round_state.target_type,
        "ref": review_round_state.target_ref,
    });
    if let (Some(object), Some(target_metadata)) = (
        payload.as_object_mut(),
        review_round_state.target_metadata.as_object(),
    ) {
        for (key, value) in target_metadata {
            object.insert(key.clone(), value.clone());
        }
    }
    payload
}

pub(crate) fn load_latest_failure_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<FailureEventState>> {
    let row: Option<(i64, String)> = transaction
        .query_row(
            r#"
            SELECT event_id, payload_json
            FROM CORE__events
            WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__loop_failed'
            ORDER BY loop_seq DESC
            LIMIT 1
            "#,
            [loop_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((event_id, payload_json)) = row else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&payload_json)?;
    Ok(Some(FailureEventState {
        event_id,
        failure_cause_type: required_str(&payload, "failure_cause_type")?.to_owned(),
        summary: required_str(&payload, "summary")?.to_owned(),
        phase_at_failure: required_str(&payload, "phase_at_failure")?.to_owned(),
        last_stable_context: payload
            .get("last_stable_context")
            .cloned()
            .unwrap_or_else(|| json!({})),
    }))
}

pub(crate) fn has_integrated_commits_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<bool> {
    transaction
        .query_row(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM CORE__events
                WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__accepted_commits_integrated'
            )
            "#,
            [loop_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn load_latest_caller_integration_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<CallerIntegrationState>> {
    let row: Option<String> = transaction
        .query_row(
            r#"
            SELECT payload_json
            FROM CORE__events
            WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__caller_integration_recorded'
            ORDER BY loop_seq DESC
            LIMIT 1
            "#,
            [loop_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload_json) = row else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&payload_json)?;
    Ok(Some(CallerIntegrationState {
        caller_branch: required_str(&payload, "caller_branch")?.to_owned(),
        final_head_sha: required_str(&payload, "final_head_sha")?.to_owned(),
        strategy: required_str(&payload, "strategy")?.to_owned(),
        landed_commit_shas: serde_json::from_value(
            payload
                .get("landed_commit_shas")
                .cloned()
                .ok_or_else(|| anyhow!("missing landed_commit_shas"))?,
        )?,
        resolution_notes: payload
            .get("resolution_notes")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }))
}

pub(crate) fn load_caller_finalize_status(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<String>> {
    transaction
        .query_row(
            r#"
            SELECT status
            FROM SUBMIT_LOOP__caller_finalize_current
            WHERE loop_id = ?1
            "#,
            [loop_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn load_latest_worktree_cleanup_warning_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<WorktreeCleanupWarningState>> {
    let row: Option<String> = transaction
        .query_row(
            r#"
            SELECT payload_json
            FROM CORE__events
            WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__worktree_cleanup_warning'
            ORDER BY loop_seq DESC
            LIMIT 1
            "#,
            [loop_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload_json) = row else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&payload_json)?;
    Ok(Some(WorktreeCleanupWarningState {
        summary: required_str(&payload, "summary")?.to_owned(),
        worktree_path: required_str(&payload, "worktree_path")?.to_owned(),
        worktree_branch: required_str(&payload, "worktree_branch")?.to_owned(),
        worktree_label: required_str(&payload, "worktree_label")?.to_owned(),
    }))
}

pub(crate) fn load_accepted_artifact_material(
    transaction: &Transaction<'_>,
    loop_id: &str,
    checkpoint_id: &str,
    commit_sha: &str,
) -> Result<AcceptedArtifactMaterial> {
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__artifact_accepted'
        ORDER BY event_id DESC
        "#,
    )?;
    let mut rows = statement.query([loop_id])?;
    while let Some(row) = rows.next()? {
        let payload_json: String = row.get(0)?;
        let payload: Value = serde_json::from_str(&payload_json)?;
        if required_str(&payload, "checkpoint_id")? == checkpoint_id
            && required_str(&payload, "candidate_commit_sha")? == commit_sha
        {
            return Ok(AcceptedArtifactMaterial {
                title: required_str(&payload, "checkpoint_title")?.to_owned(),
                change_summary: payload
                    .get("change_summary")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            });
        }
    }
    bail!(
        "missing accepted artifact summary material for checkpoint {} and commit {}",
        checkpoint_id,
        commit_sha
    )
}

pub(crate) fn load_json_content(transaction: &Transaction<'_>, content_ref: &str) -> Result<Value> {
    loopy_common_events::load_json_content(transaction, content_ref)
}

pub(crate) fn load_loop_task_summary(
    transaction: &Transaction<'_>,
    loop_input_ref: &str,
) -> Result<String> {
    let loop_input = load_json_content(transaction, loop_input_ref)?;
    required_str(&loop_input, "summary").map(str::to_owned)
}

fn load_show_loop_core(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopCoreState>> {
    transaction
        .query_row(
            r#"
            SELECT loop_id, status, phase, updated_at
            FROM SUBMIT_LOOP__loop_current
            WHERE loop_id = ?1
            "#,
            [loop_id],
            |row| {
                Ok(ShowLoopCoreState {
                    loop_id: row.get(0)?,
                    status: row.get(1)?,
                    phase: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_show_loop_freshness_token(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<String>> {
    // Pollers need a token that advances for every accepted loop event, not only
    // for the subset of transitions mirrored onto loop_current.updated_at.
    transaction
        .query_row(
            r#"
            SELECT recorded_at
            FROM CORE__events
            WHERE loop_id = ?1
            ORDER BY loop_seq DESC
            LIMIT 1
            "#,
            [loop_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn load_show_loop_bypass_sandbox(transaction: &Transaction<'_>, loop_id: &str) -> Result<bool> {
    let loop_input_ref: String = transaction.query_row(
        "SELECT loop_input_ref FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        [loop_id],
        |row| row.get(0),
    )?;
    let loop_input = load_json_content(transaction, &loop_input_ref)?;
    Ok(loop_input
        .get("bypass_sandbox")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

pub(crate) fn load_show_loop_caller_finalize(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopCallerFinalizeSummary>> {
    let row: Option<(String, Option<String>, String)> = transaction
        .query_row(
            r#"
            SELECT status, block_context_ref, updated_at
            FROM SUBMIT_LOOP__caller_finalize_current
            WHERE loop_id = ?1
            "#,
            [loop_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((status, block_context_ref, updated_at)) = row else {
        return Ok(None);
    };
    let block_context = block_context_ref
        .map(|content_ref| load_json_content(transaction, &content_ref))
        .transpose()?
        .unwrap_or_else(|| json!({}));
    Ok(Some(ShowLoopCallerFinalizeSummary {
        status,
        updated_at,
        blocking_summary: block_context
            .get("blocking_summary")
            .and_then(Value::as_str)
            .map(str::to_owned),
        human_question: block_context
            .get("human_question")
            .and_then(Value::as_str)
            .map(str::to_owned),
        conflicting_files: block_context
            .get("conflicting_files")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    }))
}

fn load_show_loop_plan(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopPlanSummary>> {
    transaction
        .query_row(
            r#"
            SELECT latest_submitted_plan_revision,
                   current_executable_plan_revision
            FROM SUBMIT_LOOP__plan_current
            WHERE loop_id = ?1
            "#,
            [loop_id],
            |row| {
                Ok(ShowLoopPlanSummary {
                    latest_submitted_plan_revision: row.get(0)?,
                    current_executable_plan_revision: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_show_loop_worktree(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopWorktreeSummary>> {
    transaction
        .query_row(
            r#"
            SELECT path, branch, label, lifecycle, updated_at
            FROM SUBMIT_LOOP__worktree_current
            WHERE loop_id = ?1
            "#,
            [loop_id],
            |row| {
                Ok(ShowLoopWorktreeSummary {
                    path: row.get(0)?,
                    branch: row.get(1)?,
                    label: row.get(2)?,
                    lifecycle: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_show_loop_latest_invocation(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopInvocationSummary>> {
    transaction
        .query_row(
            r#"
            SELECT invocation_id,
                   invocation_role,
                   stage,
                   status,
                   accepted_api,
                   review_round_id,
                   updated_at
            FROM CORE__invocation_current
            WHERE loop_id = ?1
            ORDER BY updated_at DESC, invocation_id DESC
            LIMIT 1
            "#,
            [loop_id],
            |row| {
                Ok(ShowLoopInvocationSummary {
                    invocation_id: row.get(0)?,
                    invocation_role: row.get(1)?,
                    stage: row.get(2)?,
                    status: row.get(3)?,
                    accepted_api: row.get(4)?,
                    review_round_id: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_show_loop_latest_review(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopReviewSummary>> {
    // Older databases may need the event-log fallback until `updated_at` is fully backfilled.
    if !transaction_has_text_column(transaction, "SUBMIT_LOOP__review_current", "updated_at")?
        || !review_current_updated_at_is_usable_for_loop(transaction, loop_id)?
    {
        return load_show_loop_latest_review_without_updated_at(transaction, loop_id);
    }
    transaction
        .query_row(
            r#"
            SELECT review_round_id, review_kind, round_status, target_type, target_ref
            FROM SUBMIT_LOOP__review_current
            WHERE loop_id = ?1
            ORDER BY updated_at DESC, review_round_id DESC
            LIMIT 1
            "#,
            [loop_id],
            |row| {
                Ok(ShowLoopReviewSummary {
                    review_round_id: row.get(0)?,
                    review_kind: row.get(1)?,
                    round_status: row.get(2)?,
                    target_type: row.get(3)?,
                    target_ref: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn load_show_loop_latest_review_without_updated_at(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopReviewSummary>> {
    let review_updated_at = load_review_round_updated_at_from_events(transaction, loop_id)?;
    let mut statement = transaction.prepare(
        r#"
        SELECT review_round_id, review_kind, round_status, target_type, target_ref
        FROM SUBMIT_LOOP__review_current
        WHERE loop_id = ?1
        "#,
    )?;
    let mut rows = statement.query([loop_id])?;
    let mut latest_review: Option<(String, String, ShowLoopReviewSummary)> = None;
    while let Some(row) = rows.next()? {
        let review_round_id: String = row.get(0)?;
        let summary = ShowLoopReviewSummary {
            review_round_id: review_round_id.clone(),
            review_kind: row.get(1)?,
            round_status: row.get(2)?,
            target_type: row.get(3)?,
            target_ref: row.get(4)?,
        };
        let updated_at = review_updated_at
            .get(&review_round_id)
            .cloned()
            .unwrap_or_default();
        let is_newer = match latest_review.as_ref() {
            None => true,
            Some((latest_updated_at, latest_id, _)) => {
                updated_at > *latest_updated_at
                    || (updated_at == *latest_updated_at && review_round_id > *latest_id)
            }
        };
        if is_newer {
            latest_review = Some((updated_at, review_round_id, summary));
        }
    }
    Ok(latest_review.map(|(_, _, summary)| summary))
}

fn load_review_round_updated_at_from_events(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<HashMap<String, String>> {
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json, recorded_at
        FROM CORE__events
        WHERE loop_id = ?1
          AND event_name IN (
              'SUBMIT_LOOP__review_round_opened',
              'SUBMIT_LOOP__checkpoint_review_round_recorded',
              'SUBMIT_LOOP__artifact_review_round_recorded'
          )
        ORDER BY event_id ASC
        "#,
    )?;
    let mut rows = statement.query([loop_id])?;
    let mut review_updated_at = HashMap::new();
    while let Some(row) = rows.next()? {
        let payload_json: String = row.get(0)?;
        let recorded_at: String = row.get(1)?;
        let payload: Value =
            serde_json::from_str(&payload_json).context("failed to decode review event payload")?;
        let review_round_id = required_str(&payload, "review_round_id")?;
        review_updated_at.insert(review_round_id.to_owned(), recorded_at);
    }
    Ok(review_updated_at)
}

fn transaction_has_text_column(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
) -> Result<bool> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut statement = transaction.prepare(&pragma)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let existing: String = row.get(1)?;
        if existing == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn review_current_updated_at_is_usable_for_loop(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<bool> {
    let has_blank_updated_at: bool = transaction.query_row(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM SUBMIT_LOOP__review_current
            WHERE loop_id = ?1
              AND updated_at = ''
        )
        "#,
        [loop_id],
        |row| row.get(0),
    )?;
    Ok(!has_blank_updated_at)
}

fn load_show_loop_result(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Option<ShowLoopResultSummary>> {
    transaction
        .query_row(
            "SELECT status, generated_at FROM CORE__result_current WHERE loop_id = ?1",
            [loop_id],
            |row| {
                Ok(ShowLoopResultSummary {
                    status: row.get(0)?,
                    generated_at: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn load_loop_state(transaction: &Transaction<'_>, loop_id: &str) -> Result<LoopState> {
    ensure_loop_projection_current(transaction, loop_id)?;
    transaction
        .query_row(
            r#"
            SELECT phase,
                   status,
                   base_commit_sha,
                   loop_input_ref,
                   COALESCE(resolved_role_selection_ref, loop_input_ref),
                   worktree_path,
                   worktree_branch,
                   worktree_label,
                   failure_summary
            FROM SUBMIT_LOOP__loop_current
            WHERE loop_id = ?1
            "#,
            [loop_id],
            |row| {
                Ok(LoopState {
                    phase: row.get(0)?,
                    status: row.get(1)?,
                    base_commit_sha: row.get(2)?,
                    loop_input_ref: row.get(3)?,
                    resolved_role_selection_ref: row.get(4)?,
                    worktree_path: row.get(5)?,
                    worktree_branch: row.get(6)?,
                    worktree_label: row.get(7)?,
                    failure_summary: row.get(8)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown loop_id {loop_id}"))
}

pub(crate) fn ensure_loop_status_is_open(loop_state: &LoopState, action: &str) -> Result<()> {
    if loop_state.status != "open" {
        bail!("cannot {action} when loop status is {}", loop_state.status);
    }
    Ok(())
}
