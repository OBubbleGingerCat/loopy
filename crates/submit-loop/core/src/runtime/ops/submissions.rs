// Submission ops own terminal API acceptance and review barriers; projection replay stays elsewhere.

use super::super::{projection, query, system, *};

fn authenticate_invocation_request(
    transaction: &Transaction<'_>,
    invocation_context_path: &Path,
) -> Result<AuthenticatedTerminalRequest> {
    let invocation_context_payload = system::read_json_file(invocation_context_path)
        .with_context(|| format!("failed to read {}", invocation_context_path.display()))?;
    let loop_id = required_str(&invocation_context_payload, "loop_id")?.to_owned();
    query::ensure_loop_projection_current(transaction, &loop_id)?;
    let invocation_id = required_str(&invocation_context_payload, "invocation_id")?.to_owned();
    query::ensure_invocation_projection_current(transaction, &invocation_id)?;
    let invocation_token = required_str(&invocation_context_payload, "token")?.to_owned();
    let invocation_state = query::load_invocation_state(transaction, &invocation_id)?;
    let stored_invocation_context =
        query::load_json_content(transaction, &invocation_state.invocation_context_ref)?;
    if invocation_context_payload != stored_invocation_context {
        bail!(
            "invocation context does not match authoritative snapshot for invocation {}",
            invocation_id
        );
    }
    if invocation_state.token != invocation_token {
        bail!(
            "invocation context token does not match authoritative token for invocation {}",
            invocation_id
        );
    }
    let authoritative_context_path = PathBuf::from(required_str(
        &stored_invocation_context,
        "invocation_context_path",
    )?);
    if authoritative_context_path != invocation_context_path {
        bail!(
            "invocation context path does not match authoritative snapshot for invocation {}",
            invocation_id
        );
    }
    Ok(AuthenticatedTerminalRequest {
        loop_id: invocation_state.loop_id.clone(),
        invocation_id,
        invocation_state,
        stored_invocation_context,
    })
}

fn authenticate_terminal_submission(
    transaction: &Transaction<'_>,
    invocation_context_path: &Path,
    accepted_api: &str,
) -> Result<AuthenticatedTerminalRequest> {
    let auth = authenticate_invocation_request(transaction, invocation_context_path)?;
    if !auth
        .invocation_state
        .allowed_terminal_apis
        .iter()
        .any(|api_name| api_name == accepted_api)
    {
        bail!(
            "invocation {} is not allowed to call {}",
            auth.invocation_id,
            accepted_api
        );
    }
    if auth.invocation_state.token_state != "consumed" {
        let loop_state = query::load_loop_state(transaction, &auth.invocation_state.loop_id)?;
        query::ensure_loop_status_is_open(&loop_state, "accept terminal submissions")?;
        if query::load_caller_finalize_status(transaction, &auth.invocation_state.loop_id)?
            .is_some()
        {
            bail!("cannot accept terminal submissions after caller finalize handoff");
        }
    }
    Ok(auth)
}

pub(crate) fn request_timeout_extension(
    runtime: &Runtime,
    request: RequestTimeoutExtensionRequest,
) -> Result<RequestTimeoutExtensionResponse> {
    super::ensure_timeout_extension_request_shape(
        request.requested_timeout_sec,
        &request.progress_summary,
        &request.rationale,
    )?;

    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let auth = authenticate_invocation_request(&transaction, &request.invocation_context_path)?;
    if auth.invocation_state.token_state != "available" {
        bail!(
            "invocation {} can no longer request a timeout extension",
            auth.invocation_id
        );
    }
    if auth.invocation_state.accepted_api.is_some() {
        bail!(
            "invocation {} already accepted a terminal API and cannot request a timeout extension",
            auth.invocation_id
        );
    }
    if auth.invocation_state.status != "running" && auth.invocation_state.status != "opened" {
        bail!(
            "invocation {} is not active and cannot request a timeout extension",
            auth.invocation_id
        );
    }
    let loop_state = query::load_loop_state(&transaction, &auth.loop_id)?;
    query::ensure_loop_status_is_open(&loop_state, "record timeout extension requests")?;
    if query::load_caller_finalize_status(&transaction, &auth.loop_id)?.is_some() {
        bail!("cannot request timeout extensions after caller finalize handoff");
    }

    let request_content_ref = projection::store_json_content(
        &transaction,
        "timeout_extension_request",
        &json!({
            "requested_timeout_sec": request.requested_timeout_sec,
            "progress_summary": request.progress_summary,
            "rationale": request.rationale,
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "SUBMIT_LOOP__timeout_extension_requested",
        &json!({
            "invocation_id": auth.invocation_id,
            "request_content_ref": request_content_ref,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &auth.invocation_id,
        &auth.invocation_state.invocation_role,
        "timeout_extension_request",
        Some("requested timeout extension"),
        &request_content_ref,
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(RequestTimeoutExtensionResponse {
        loop_id: auth.loop_id,
        invocation_id: auth.invocation_id,
        requested_timeout_sec: request.requested_timeout_sec,
        progress_summary: request.progress_summary,
        rationale: request.rationale,
    })
}

fn idempotent_terminal_response(
    invocation_state: &InvocationState,
    accepted_api: &str,
    submission_id: &str,
) -> Result<Option<bool>> {
    // A consumed token may replay the exact same submission, but it must reject any new terminal side effect.
    if invocation_state.token_state != "consumed" {
        return Ok(None);
    }
    if invocation_state.accepted_api.as_deref() == Some(accepted_api)
        && invocation_state.accepted_submission_id.as_deref() == Some(submission_id)
    {
        return Ok(Some(true));
    }
    bail!("invocation already consumed by a different terminal submission");
}

pub(crate) fn submit_checkpoint_plan(
    runtime: &Runtime,
    request: SubmitCheckpointPlanRequest,
) -> Result<SubmitCheckpointPlanResponse> {
    let invocation_context_payload = system::read_json_file(&request.invocation_context_path)
        .with_context(|| {
            format!(
                "failed to read {}",
                request.invocation_context_path.display()
            )
        })?;
    let invocation_id = required_str(&invocation_context_payload, "invocation_id")?.to_owned();
    let invocation_token = required_str(&invocation_context_payload, "token")?.to_owned();
    if request.checkpoints.is_empty() {
        bail!("checkpoint plan submission requires at least one checkpoint");
    }
    for checkpoint in &request.checkpoints {
        validate_checkpoint_plan_item(checkpoint)?;
    }

    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let invocation_state = query::load_invocation_state(&transaction, &invocation_id)?;
    let stored_invocation_context =
        query::load_json_content(&transaction, &invocation_state.invocation_context_ref)?;
    if invocation_context_payload != stored_invocation_context {
        bail!(
            "invocation context does not match authoritative snapshot for invocation {}",
            invocation_id
        );
    }
    if invocation_state.token != invocation_token {
        bail!(
            "invocation context token does not match authoritative token for invocation {}",
            invocation_id
        );
    }
    let authoritative_context_path = PathBuf::from(required_str(
        &stored_invocation_context,
        "invocation_context_path",
    )?);
    if authoritative_context_path != request.invocation_context_path {
        bail!(
            "invocation context path does not match authoritative snapshot for invocation {}",
            invocation_id
        );
    }
    if invocation_state.stage != "planning"
        || !invocation_state
            .allowed_terminal_apis
            .iter()
            .any(|api_name| api_name == "SUBMIT_LOOP__submit_checkpoint_plan")
    {
        bail!(
            "invocation {} is not allowed to submit a checkpoint plan",
            invocation_id
        );
    }
    let loop_id = invocation_state.loop_id.clone();

    if invocation_state.token_state == "consumed" {
        if invocation_state.accepted_api.as_deref() == Some("SUBMIT_LOOP__submit_checkpoint_plan")
            && invocation_state.accepted_submission_id.as_deref()
                == Some(request.submission_id.as_str())
        {
            let plan_revision = query::load_submitted_plan_revision(
                &transaction,
                &loop_id,
                &invocation_id,
                request.submission_id.as_str(),
            )?
            .ok_or_else(|| {
                anyhow!("idempotent checkpoint plan submission is missing plan revision")
            })?;
            transaction.commit()?;
            return Ok(SubmitCheckpointPlanResponse {
                loop_id,
                invocation_id,
                submission_id: request.submission_id,
                accepted_api: "SUBMIT_LOOP__submit_checkpoint_plan".to_owned(),
                plan_revision,
                idempotent: true,
            });
        }
        bail!(
            "invocation {} already consumed by a different terminal submission",
            invocation_id
        );
    }
    let loop_state = query::load_loop_state(&transaction, &loop_id)?;
    if loop_state.status != "open" {
        bail!(
            "cannot submit checkpoint plan when loop status is {}",
            loop_state.status
        );
    }
    if loop_state.phase != "awaiting_worktree" && loop_state.phase != "planning" {
        bail!(
            "cannot submit checkpoint plan when loop phase is {}",
            loop_state.phase
        );
    }

    let improvement_opportunities = validate_review_issue_bucket(
        request.improvement_opportunities.as_deref().unwrap_or(&[]),
        "improvement_opportunities",
        &["summary", "rationale", "suggested_follow_up"],
    )?;

    let plan_revision =
        query::load_latest_submitted_plan_revision(&transaction, &loop_id)?.unwrap_or(0) + 1;
    let checkpoint_specs = request
        .checkpoints
        .iter()
        .enumerate()
        .map(|(sequence_index, checkpoint)| {
            json!({
                "checkpoint_id": format!("checkpoint-{}", Uuid::now_v7()),
                "sequence_index": sequence_index,
                "title": &checkpoint.title,
                "kind": &checkpoint.kind,
                "deliverables": &checkpoint.deliverables,
                "acceptance": &checkpoint.acceptance,
                "revision": 1,
            })
        })
        .collect::<Vec<_>>();
    let submission_content_ref = projection::store_json_content(
        &transaction,
        "submission_content",
        &json!({
            "checkpoints": request.checkpoints,
            "improvement_opportunities": improvement_opportunities,
            "notes": request.notes,
        }),
    )?;
    projection::append_event(
        &transaction,
        &loop_id,
        "CORE__terminal_api_called",
        &json!({
            "invocation_id": invocation_id,
            "submission_id": request.submission_id,
            "api_name": "SUBMIT_LOOP__submit_checkpoint_plan",
            "status": "accepted",
        }),
    )?;
    projection::append_event(
        &transaction,
        &loop_id,
        "SUBMIT_LOOP__plan_submitted",
        &json!({
            "invocation_id": invocation_id,
            "submission_id": request.submission_id,
            "plan_revision": plan_revision,
            "submission_content_ref": submission_content_ref,
            "checkpoints": checkpoint_specs,
            "notes": request.notes,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &invocation_id,
        "worker",
        "terminal_submission",
        Some("submitted checkpoint plan"),
        &submission_content_ref,
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(SubmitCheckpointPlanResponse {
        loop_id,
        invocation_id,
        submission_id: request.submission_id,
        accepted_api: "SUBMIT_LOOP__submit_checkpoint_plan".to_owned(),
        plan_revision,
        idempotent: false,
    })
}

fn validate_checkpoint_plan_item(checkpoint: &CheckpointPlanItem) -> Result<()> {
    if checkpoint.title.trim().is_empty() {
        bail!("checkpoint title must not be blank");
    }
    if checkpoint.kind != "artifact" {
        bail!(
            "unsupported checkpoint kind {}; only artifact checkpoints are currently allowed",
            checkpoint.kind
        );
    }
    if checkpoint.deliverables.is_empty() {
        bail!(
            "checkpoint {} must declare at least one deliverable",
            checkpoint.title
        );
    }
    for deliverable in &checkpoint.deliverables {
        if deliverable.path.trim().is_empty() {
            bail!(
                "checkpoint {} contains a deliverable with a blank path",
                checkpoint.title
            );
        }
        if deliverable.deliverable_type != "file" {
            bail!(
                "checkpoint {} contains unsupported deliverable type {}; only file deliverables are currently allowed",
                checkpoint.title,
                deliverable.deliverable_type
            );
        }
    }
    if checkpoint.acceptance.verification_steps.is_empty() {
        bail!(
            "checkpoint {} must declare at least one acceptance.verification_steps entry",
            checkpoint.title
        );
    }
    if checkpoint.acceptance.expected_outcomes.is_empty() {
        bail!(
            "checkpoint {} must declare at least one acceptance.expected_outcomes entry",
            checkpoint.title
        );
    }
    if checkpoint
        .acceptance
        .verification_steps
        .iter()
        .any(|step| step.trim().is_empty())
    {
        bail!(
            "checkpoint {} contains a blank acceptance.verification_steps entry",
            checkpoint.title
        );
    }
    if checkpoint
        .acceptance
        .expected_outcomes
        .iter()
        .any(|outcome| outcome.trim().is_empty())
    {
        bail!(
            "checkpoint {} contains a blank acceptance.expected_outcomes entry",
            checkpoint.title
        );
    }
    Ok(())
}

pub(crate) fn submit_checkpoint_review(
    runtime: &Runtime,
    request: SubmitCheckpointReviewRequest,
) -> Result<SubmitCheckpointReviewResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let auth = authenticate_terminal_submission(
        &transaction,
        &request.invocation_context_path,
        "SUBMIT_LOOP__submit_checkpoint_review",
    )?;
    if let Some(response) = idempotent_terminal_response(
        &auth.invocation_state,
        "SUBMIT_LOOP__submit_checkpoint_review",
        request.submission_id.as_str(),
    )? {
        transaction.commit()?;
        return Ok(TerminalSubmissionResponse {
            loop_id: auth.loop_id,
            invocation_id: auth.invocation_id,
            submission_id: request.submission_id,
            accepted_api: "SUBMIT_LOOP__submit_checkpoint_review".to_owned(),
            idempotent: response,
        });
    }
    let SubmitCheckpointReviewRequest {
        invocation_context_path: _,
        submission_id,
        decision,
        summary,
        blocking_issues,
        nonblocking_issues,
        improvement_opportunities,
        notes,
    } = request;

    let review_round_id = auth
        .invocation_state
        .review_round_id
        .clone()
        .ok_or_else(|| anyhow!("checkpoint review invocation missing review_round_id"))?;
    let review_slot_id = auth
        .invocation_state
        .review_slot_id
        .clone()
        .ok_or_else(|| anyhow!("checkpoint review invocation missing review_slot_id"))?;
    let review_round_state =
        query::load_review_round_state(&transaction, &auth.loop_id, &review_round_id)?;
    if review_round_state.review_kind != "checkpoint" {
        bail!(
            "review round {} does not accept checkpoint reviews",
            review_round_id
        );
    }
    if summary.trim().is_empty() {
        bail!("review summary must not be blank");
    }
    let decision = parse_review_decision(&decision)?;
    let blocking_issues = validate_review_issue_bucket(
        &blocking_issues,
        "blocking_issues",
        &["summary", "rationale", "expected_revision"],
    )?;
    ensure_review_decision_is_consistent(&decision, &blocking_issues)?;
    let nonblocking_issues = validate_review_issue_bucket(
        nonblocking_issues.as_deref().unwrap_or(&[]),
        "nonblocking_issues",
        &["summary", "rationale", "expected_revision"],
    )?;
    let improvement_opportunities = validate_review_issue_bucket(
        improvement_opportunities.as_deref().unwrap_or(&[]),
        "improvement_opportunities",
        &["summary", "rationale", "suggested_follow_up"],
    )?;
    let submission_content_ref = projection::store_json_content(
        &transaction,
        "submission_content",
        &json!({
            "decision": decision,
            "summary": summary,
            "blocking_issues": blocking_issues,
            "nonblocking_issues": nonblocking_issues,
            "improvement_opportunities": improvement_opportunities,
            "notes": notes,
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "CORE__terminal_api_called",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "api_name": "SUBMIT_LOOP__submit_checkpoint_review",
            "status": "accepted",
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "SUBMIT_LOOP__checkpoint_review_submitted",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "review_round_id": review_round_id,
            "review_slot_id": review_slot_id,
            "decision": decision,
            "submission_content_ref": submission_content_ref,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &auth.invocation_id,
        "reviewer",
        "terminal_submission",
        Some("submitted checkpoint review"),
        &submission_content_ref,
    )?;
    maybe_append_review_barrier_events(
        &transaction,
        &auth.loop_id,
        &review_round_state,
        &review_round_id,
        &review_slot_id,
        ReviewSlotTerminal::Decision {
            decision: decision.to_owned(),
            submission_content_ref,
        },
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(TerminalSubmissionResponse {
        loop_id: auth.loop_id,
        invocation_id: auth.invocation_id,
        submission_id,
        accepted_api: "SUBMIT_LOOP__submit_checkpoint_review".to_owned(),
        idempotent: false,
    })
}

pub(crate) fn submit_artifact_review(
    runtime: &Runtime,
    request: SubmitArtifactReviewRequest,
) -> Result<SubmitArtifactReviewResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let auth = authenticate_terminal_submission(
        &transaction,
        &request.invocation_context_path,
        "SUBMIT_LOOP__submit_artifact_review",
    )?;
    if let Some(response) = idempotent_terminal_response(
        &auth.invocation_state,
        "SUBMIT_LOOP__submit_artifact_review",
        request.submission_id.as_str(),
    )? {
        transaction.commit()?;
        return Ok(TerminalSubmissionResponse {
            loop_id: auth.loop_id,
            invocation_id: auth.invocation_id,
            submission_id: request.submission_id,
            accepted_api: "SUBMIT_LOOP__submit_artifact_review".to_owned(),
            idempotent: response,
        });
    }
    let SubmitArtifactReviewRequest {
        invocation_context_path: _,
        submission_id,
        decision,
        summary,
        blocking_issues,
        nonblocking_issues,
        improvement_opportunities,
        notes,
    } = request;

    let review_round_id = auth
        .invocation_state
        .review_round_id
        .clone()
        .ok_or_else(|| anyhow!("artifact review invocation missing review_round_id"))?;
    let review_slot_id = auth
        .invocation_state
        .review_slot_id
        .clone()
        .ok_or_else(|| anyhow!("artifact review invocation missing review_slot_id"))?;
    let review_round_state =
        query::load_review_round_state(&transaction, &auth.loop_id, &review_round_id)?;
    if review_round_state.review_kind != "artifact" {
        bail!(
            "review round {} does not accept artifact reviews",
            review_round_id
        );
    }
    if summary.trim().is_empty() {
        bail!("review summary must not be blank");
    }
    let decision = parse_review_decision(&decision)?;
    let blocking_issues = validate_review_issue_bucket(
        &blocking_issues,
        "blocking_issues",
        &["summary", "rationale", "expected_revision"],
    )?;
    ensure_review_decision_is_consistent(&decision, &blocking_issues)?;
    let nonblocking_issues = validate_review_issue_bucket(
        nonblocking_issues.as_deref().unwrap_or(&[]),
        "nonblocking_issues",
        &["summary", "rationale", "expected_revision"],
    )?;
    let improvement_opportunities = validate_review_issue_bucket(
        improvement_opportunities.as_deref().unwrap_or(&[]),
        "improvement_opportunities",
        &["summary", "rationale", "suggested_follow_up"],
    )?;
    let submission_content_ref = projection::store_json_content(
        &transaction,
        "submission_content",
        &json!({
            "decision": decision,
            "summary": summary,
            "blocking_issues": blocking_issues,
            "nonblocking_issues": nonblocking_issues,
            "improvement_opportunities": improvement_opportunities,
            "notes": notes,
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "CORE__terminal_api_called",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "api_name": "SUBMIT_LOOP__submit_artifact_review",
            "status": "accepted",
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "SUBMIT_LOOP__artifact_review_submitted",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "review_round_id": review_round_id,
            "review_slot_id": review_slot_id,
            "decision": decision,
            "submission_content_ref": submission_content_ref,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &auth.invocation_id,
        "reviewer",
        "terminal_submission",
        Some("submitted artifact review"),
        &submission_content_ref,
    )?;
    maybe_append_review_barrier_events(
        &transaction,
        &auth.loop_id,
        &review_round_state,
        &review_round_id,
        &review_slot_id,
        ReviewSlotTerminal::Decision {
            decision: decision.to_owned(),
            submission_content_ref,
        },
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(TerminalSubmissionResponse {
        loop_id: auth.loop_id,
        invocation_id: auth.invocation_id,
        submission_id,
        accepted_api: "SUBMIT_LOOP__submit_artifact_review".to_owned(),
        idempotent: false,
    })
}

pub(crate) fn submit_candidate_commit(
    runtime: &Runtime,
    request: SubmitCandidateCommitRequest,
) -> Result<SubmitCandidateCommitResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let auth = authenticate_terminal_submission(
        &transaction,
        &request.invocation_context_path,
        "SUBMIT_LOOP__submit_candidate_commit",
    )?;
    if let Some(response) = idempotent_terminal_response(
        &auth.invocation_state,
        "SUBMIT_LOOP__submit_candidate_commit",
        request.submission_id.as_str(),
    )? {
        transaction.commit()?;
        return Ok(TerminalSubmissionResponse {
            loop_id: auth.loop_id,
            invocation_id: auth.invocation_id,
            submission_id: request.submission_id,
            accepted_api: "SUBMIT_LOOP__submit_candidate_commit".to_owned(),
            idempotent: response,
        });
    }
    if auth.invocation_state.invocation_role != "worker"
        || auth.invocation_state.stage != "artifact"
    {
        bail!(
            "invocation {} is not allowed to submit a candidate commit",
            auth.invocation_id
        );
    }
    let checkpoint_id = required_str(&auth.stored_invocation_context, "checkpoint_id")?;
    validate_candidate_commit(
        runtime,
        &transaction,
        &auth.loop_id,
        checkpoint_id,
        &request.candidate_commit_sha,
    )?;
    let improvement_opportunities = validate_review_issue_bucket(
        request.improvement_opportunities.as_deref().unwrap_or(&[]),
        "improvement_opportunities",
        &["summary", "rationale", "suggested_follow_up"],
    )?;
    let submission_content_ref = projection::store_json_content(
        &transaction,
        "submission_content",
        &json!({
            "candidate_commit_sha": request.candidate_commit_sha,
            "change_summary": request.change_summary,
            "improvement_opportunities": improvement_opportunities,
            "notes": request.notes,
            "checkpoint_id": checkpoint_id,
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "CORE__terminal_api_called",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": request.submission_id,
            "api_name": "SUBMIT_LOOP__submit_candidate_commit",
            "status": "accepted",
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "SUBMIT_LOOP__candidate_commit_submitted",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": request.submission_id,
            "checkpoint_id": checkpoint_id,
            "candidate_commit_sha": request.candidate_commit_sha,
            "submission_content_ref": submission_content_ref,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &auth.invocation_id,
        "worker",
        "terminal_submission",
        Some("submitted candidate commit"),
        &submission_content_ref,
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(TerminalSubmissionResponse {
        loop_id: auth.loop_id,
        invocation_id: auth.invocation_id,
        submission_id: request.submission_id,
        accepted_api: "SUBMIT_LOOP__submit_candidate_commit".to_owned(),
        idempotent: false,
    })
}

pub(crate) fn declare_worker_blocked(
    runtime: &Runtime,
    request: DeclareWorkerBlockedRequest,
) -> Result<DeclareWorkerBlockedResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let auth = authenticate_terminal_submission(
        &transaction,
        &request.invocation_context_path,
        "SUBMIT_LOOP__declare_worker_blocked",
    )?;
    if let Some(response) = idempotent_terminal_response(
        &auth.invocation_state,
        "SUBMIT_LOOP__declare_worker_blocked",
        request.submission_id.as_str(),
    )? {
        let existing_result_ref = query::load_existing_result_ref(&transaction, &auth.loop_id)?
            .ok_or_else(|| {
                anyhow!("blocked worker submission was accepted but no result was materialized")
            })?;
        let existing_result = query::load_json_content(&transaction, &existing_result_ref)?;
        transaction.commit()?;
        debug_assert!(response);
        return Ok(existing_result);
    }
    if auth.invocation_state.invocation_role != "worker" {
        bail!(
            "invocation {} is not a worker invocation",
            auth.invocation_id
        );
    }
    let DeclareWorkerBlockedRequest {
        invocation_context_path: _,
        submission_id,
        summary,
        rationale,
        why_unrecoverable,
        notes,
    } = request;
    if summary.trim().is_empty()
        || rationale.trim().is_empty()
        || why_unrecoverable.trim().is_empty()
    {
        bail!("blocked submissions require non-empty summary, rationale, and why_unrecoverable");
    }
    let mut blocked_content = json!({
        "summary": summary,
        "rationale": rationale,
        "why_unrecoverable": why_unrecoverable,
    });
    if let Some(notes) = notes {
        blocked_content["notes"] = Value::String(notes);
    }
    let blocked_content_ref =
        projection::store_json_content(&transaction, "submission_content", &blocked_content)?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "CORE__terminal_api_called",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "api_name": "SUBMIT_LOOP__declare_worker_blocked",
            "status": "accepted",
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "SUBMIT_LOOP__worker_blocked_accepted",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "submission_content_ref": blocked_content_ref,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &auth.invocation_id,
        "worker",
        "terminal_submission",
        Some("declared worker blocked"),
        &blocked_content_ref,
    )?;
    let loop_state = query::load_loop_state(&transaction, &auth.loop_id)?;
    let result = super::r#loop::append_failure_result(
        &transaction,
        &auth.loop_id,
        &loop_state,
        "worker_blocked",
        &summary,
        &loop_state.phase,
        &json!({
            "base_commit_sha": loop_state.base_commit_sha,
            "worktree_branch": loop_state.worktree_branch,
            "worktree_label": loop_state.worktree_label,
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(result)
}

pub(crate) fn declare_review_blocked(
    runtime: &Runtime,
    request: DeclareReviewBlockedRequest,
) -> Result<DeclareReviewBlockedResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let auth = authenticate_terminal_submission(
        &transaction,
        &request.invocation_context_path,
        "SUBMIT_LOOP__declare_review_blocked",
    )?;
    if let Some(response) = idempotent_terminal_response(
        &auth.invocation_state,
        "SUBMIT_LOOP__declare_review_blocked",
        request.submission_id.as_str(),
    )? {
        transaction.commit()?;
        return Ok(TerminalSubmissionResponse {
            loop_id: auth.loop_id,
            invocation_id: auth.invocation_id,
            submission_id: request.submission_id,
            accepted_api: "SUBMIT_LOOP__declare_review_blocked".to_owned(),
            idempotent: response,
        });
    }
    let DeclareReviewBlockedRequest {
        invocation_context_path: _,
        submission_id,
        summary,
        rationale,
        why_unrecoverable,
        notes,
    } = request;
    let review_round_id = auth
        .invocation_state
        .review_round_id
        .clone()
        .ok_or_else(|| anyhow!("reviewer invocation missing review_round_id"))?;
    let review_slot_id = auth
        .invocation_state
        .review_slot_id
        .clone()
        .ok_or_else(|| anyhow!("reviewer invocation missing review_slot_id"))?;
    let review_round_state =
        query::load_review_round_state(&transaction, &auth.loop_id, &review_round_id)?;
    if summary.trim().is_empty()
        || rationale.trim().is_empty()
        || why_unrecoverable.trim().is_empty()
    {
        bail!("blocked submissions require non-empty summary, rationale, and why_unrecoverable");
    }
    let mut blocked_content = json!({
        "summary": summary,
        "rationale": rationale,
        "why_unrecoverable": why_unrecoverable,
    });
    if let Some(notes) = notes {
        blocked_content["notes"] = Value::String(notes);
    }
    let blocked_content_ref =
        projection::store_json_content(&transaction, "submission_content", &blocked_content)?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "CORE__terminal_api_called",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "api_name": "SUBMIT_LOOP__declare_review_blocked",
            "status": "accepted",
        }),
    )?;
    projection::append_event(
        &transaction,
        &auth.loop_id,
        "SUBMIT_LOOP__review_blocked_recorded",
        &json!({
            "invocation_id": auth.invocation_id,
            "submission_id": submission_id,
            "review_round_id": review_round_id,
            "review_slot_id": review_slot_id,
            "submission_content_ref": blocked_content_ref,
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        &auth.invocation_id,
        "reviewer",
        "terminal_submission",
        Some("declared review blocked"),
        &blocked_content_ref,
    )?;
    maybe_append_review_barrier_events(
        &transaction,
        &auth.loop_id,
        &review_round_state,
        &review_round_id,
        &review_slot_id,
        ReviewSlotTerminal::Blocked {
            submission_content_ref: blocked_content_ref,
        },
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(TerminalSubmissionResponse {
        loop_id: auth.loop_id,
        invocation_id: auth.invocation_id,
        submission_id,
        accepted_api: "SUBMIT_LOOP__declare_review_blocked".to_owned(),
        idempotent: false,
    })
}

fn parse_review_decision(decision: &str) -> Result<&str> {
    match decision {
        "approve" | "reject" => Ok(decision),
        other => bail!("unsupported review decision {other}"),
    }
}

fn validate_review_issue_bucket(
    values: &[Value],
    bucket_name: &str,
    required_fields: &[&str],
) -> Result<Vec<Value>> {
    values
        .iter()
        .cloned()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow!("{bucket_name} entries must be JSON objects"))?;
            for field_name in required_fields {
                let valid = object
                    .get(*field_name)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_some();
                if !valid {
                    return Err(anyhow!(
                        "{bucket_name} entries must include non-empty string field {field_name}"
                    ));
                }
            }
            Ok(Value::Object(object.clone()))
        })
        .collect()
}

fn ensure_review_decision_is_consistent(decision: &str, blocking_issues: &[Value]) -> Result<()> {
    if decision == "approve" && !blocking_issues.is_empty() {
        bail!("approve reviews cannot include blocking_issues");
    }
    Ok(())
}

fn compute_review_slot_state(
    review_round_state: &ReviewRoundState,
    review_slot_id: &str,
    terminal: &ReviewSlotTerminal,
) -> Result<Vec<ReviewSlotState>> {
    let mut found = false;
    let next_slot_state = review_round_state
        .slot_state
        .iter()
        .cloned()
        .map(|mut slot| {
            if slot.review_slot_id == review_slot_id {
                found = true;
                if slot.status != "pending" {
                    return Err(anyhow!(
                        "review slot {} is not pending in review round",
                        review_slot_id
                    ));
                }
                match terminal {
                    ReviewSlotTerminal::Decision {
                        decision,
                        submission_content_ref,
                    } => {
                        slot.status = decision.clone();
                        slot.decision = Some(decision.clone());
                        slot.submission_content_ref = Some(submission_content_ref.clone());
                    }
                    ReviewSlotTerminal::Blocked {
                        submission_content_ref,
                    } => {
                        slot.status = "blocked".to_owned();
                        slot.decision = None;
                        slot.submission_content_ref = Some(submission_content_ref.clone());
                    }
                }
            }
            Ok(slot)
        })
        .collect::<Result<Vec<_>>>()?;
    if !found {
        bail!("review slot {} not found in review round", review_slot_id);
    }
    Ok(next_slot_state)
}

fn append_review_round_status_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
    review_round_id: &str,
    review_kind: &str,
    target_type: &str,
    target_ref: &str,
    target_metadata: &Value,
    slot_state: &[ReviewSlotState],
    round_status: &str,
) -> Result<()> {
    let event_name = match review_kind {
        "checkpoint" => "SUBMIT_LOOP__checkpoint_review_round_recorded",
        "artifact" => "SUBMIT_LOOP__artifact_review_round_recorded",
        other => bail!("unsupported review kind {other}"),
    };
    projection::append_event(
        transaction,
        loop_id,
        event_name,
        &json!({
            "review_round_id": review_round_id,
            "review_kind": review_kind,
            "target_type": target_type,
            "target_ref": target_ref,
            "target_metadata": target_metadata,
            "round_status": round_status,
            "slot_state": slot_state,
        }),
    )?;
    Ok(())
}

fn maybe_append_review_barrier_events(
    transaction: &Transaction<'_>,
    loop_id: &str,
    review_round_state: &ReviewRoundState,
    review_round_id: &str,
    review_slot_id: &str,
    terminal: ReviewSlotTerminal,
) -> Result<()> {
    let mut slot_state = compute_review_slot_state(review_round_state, review_slot_id, &terminal)?;
    if slot_state.iter().any(|slot| slot.status == "blocked") {
        close_pending_review_slots_after_block(&mut slot_state);
    }
    let all_terminal = slot_state.iter().all(|slot| slot.status != "pending");
    let any_reject = slot_state.iter().any(|slot| slot.status == "reject");
    let any_blocked = slot_state.iter().any(|slot| slot.status == "blocked");
    let all_approve = all_terminal && slot_state.iter().all(|slot| slot.status == "approve");
    let round_status = if any_blocked {
        "blocked"
    } else if !all_terminal {
        "pending"
    } else if any_reject {
        "rejected"
    } else if all_approve {
        "approved"
    } else {
        "pending"
    };

    append_review_round_status_event(
        transaction,
        loop_id,
        review_round_id,
        &review_round_state.review_kind,
        &review_round_state.target_type,
        &review_round_state.target_ref,
        &review_round_state.target_metadata,
        &slot_state,
        round_status,
    )?;

    if any_blocked {
        let blocked_summary = load_review_blocked_summary(transaction, &slot_state)?;
        let loop_state = query::load_loop_state(transaction, loop_id)?;
        super::r#loop::append_failure_result(
            transaction,
            loop_id,
            &loop_state,
            "review_blocked",
            &blocked_summary,
            &loop_state.phase,
            &json!({
                "review_kind": review_round_state.review_kind,
                "review_round_id": review_round_id,
                "target_type": review_round_state.target_type,
                "target_ref": review_round_state.target_ref,
            }),
        )?;
        return Ok(());
    }

    if !all_terminal {
        return Ok(());
    }

    if review_round_state.review_kind == "checkpoint" {
        let plan_revision = query::parse_plan_revision_target(
            &review_round_state.target_type,
            &review_round_state.target_ref,
        )?;
        if all_approve {
            let submission =
                query::load_plan_submission_for_revision(transaction, loop_id, plan_revision)?;
            let checkpoints = submission
                .get("checkpoints")
                .cloned()
                .ok_or_else(|| anyhow!("submitted plan missing checkpoints"))?;
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__plan_accepted",
                &json!({
                    "review_round_id": review_round_id,
                    "plan_revision": plan_revision,
                    "checkpoints": checkpoints,
                }),
            )?;
        } else if any_reject {
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__plan_rejected",
                &json!({
                    "review_round_id": review_round_id,
                    "plan_revision": plan_revision,
                }),
            )?;
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__attempt_consumed",
                &json!({
                    "owner": "planning_review",
                    "owner_ref": review_round_id,
                    "reason": "plan_rejected",
                    "plan_revision": plan_revision,
                }),
            )?;
        }
        return Ok(());
    }

    if review_round_state.review_kind == "artifact" {
        let candidate = load_candidate_commit_state(transaction, loop_id, review_round_state)?;
        if all_approve {
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__artifact_accepted",
                &json!({
                    "review_round_id": review_round_id,
                    "checkpoint_id": candidate.checkpoint_id,
                    "checkpoint_title": candidate.title,
                    "candidate_commit_sha": candidate.commit_sha,
                    "change_summary": candidate.change_summary,
                }),
            )?;
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__accepted_commit_recorded",
                &json!({
                    "checkpoint_id": candidate.checkpoint_id,
                    "commit_sha": candidate.commit_sha,
                }),
            )?;
        } else if any_reject {
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__artifact_rejected",
                &json!({
                    "review_round_id": review_round_id,
                    "checkpoint_id": candidate.checkpoint_id,
                    "candidate_commit_sha": candidate.commit_sha,
                }),
            )?;
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__candidate_commit_revoked",
                &json!({
                    "checkpoint_id": candidate.checkpoint_id,
                    "commit_sha": candidate.commit_sha,
                }),
            )?;
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__attempt_consumed",
                &json!({
                    "owner": "artifact_review",
                    "owner_ref": review_round_id,
                    "reason": "artifact_rejected",
                    "checkpoint_id": candidate.checkpoint_id,
                    "candidate_commit_sha": candidate.commit_sha,
                }),
            )?;
        }
        return Ok(());
    }

    bail!(
        "unsupported review kind {} for barrier aggregation",
        review_round_state.review_kind
    )
}

fn close_pending_review_slots_after_block(slot_state: &mut [ReviewSlotState]) {
    for slot in slot_state.iter_mut() {
        if slot.status == "pending" {
            slot.status = "blocked".to_owned();
            slot.decision = None;
            slot.submission_content_ref = None;
        }
    }
}

fn load_review_blocked_summary(
    transaction: &Transaction<'_>,
    slot_state: &[ReviewSlotState],
) -> Result<String> {
    let blocked_ref = slot_state
        .iter()
        .filter(|slot| slot.status == "blocked")
        .find_map(|slot| slot.submission_content_ref.as_deref())
        .ok_or_else(|| anyhow!("blocked review round is missing blocked submission content"))?;
    let submission = query::load_json_content(transaction, blocked_ref)?;
    if let Some(summary) = submission
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(summary.to_owned());
    }
    Ok("reviewer declared the loop blocked".to_owned())
}

fn load_candidate_commit_state(
    transaction: &Transaction<'_>,
    loop_id: &str,
    review_round_state: &ReviewRoundState,
) -> Result<CandidateCommitState> {
    if review_round_state.target_type != "checkpoint_id" {
        bail!(
            "unsupported artifact review target_type {}",
            review_round_state.target_type
        );
    }
    let checkpoint_id = review_round_state
        .target_metadata
        .get("checkpoint_id")
        .and_then(Value::as_str)
        .unwrap_or(review_round_state.target_ref.as_str());
    let commit_sha = review_round_state
        .target_metadata
        .get("candidate_commit_sha")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("artifact review round is missing candidate commit snapshot"))?
        .to_owned();
    let checkpoint = query::load_checkpoint_state(transaction, loop_id, checkpoint_id)?;
    let mut statement = transaction.prepare(
        r#"
        SELECT payload_json
        FROM CORE__events
        WHERE loop_id = ?1 AND event_name = 'SUBMIT_LOOP__candidate_commit_submitted'
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
            let submission_content_ref = required_str(&payload, "submission_content_ref")?;
            let submission = query::load_json_content(transaction, submission_content_ref)?;
            return Ok(CandidateCommitState {
                checkpoint_id: checkpoint.checkpoint_id,
                title: checkpoint.title,
                commit_sha,
                change_summary: submission
                    .get("change_summary")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            });
        }
    }
    bail!(
        "missing candidate commit submission content for checkpoint {}",
        checkpoint_id
    )
}

fn validate_candidate_commit(
    runtime: &Runtime,
    transaction: &Transaction<'_>,
    loop_id: &str,
    checkpoint_id: &str,
    candidate_commit_sha: &str,
) -> Result<()> {
    let loop_state = query::load_loop_state(transaction, loop_id)?;
    let checkpoint = query::load_checkpoint_state(transaction, loop_id, checkpoint_id)?;
    let authoritative_git_dir = super::r#loop::authoritative_worktree_git_dir(
        runtime,
        &loop_state.worktree_path,
        &loop_state.worktree_label,
    )?;
    let git_dir_arg = format!("--git-dir={}", authoritative_git_dir.display());
    if checkpoint.execution_state == "candidate_review" {
        bail!(
            "checkpoint {} is already in candidate_review and cannot accept a replacement candidate",
            checkpoint.checkpoint_id
        );
    }
    let expected_base = if checkpoint.sequence_index == 0 {
        loop_state.base_commit_sha
    } else {
        let previous =
            query::load_previous_checkpoint_state(transaction, loop_id, checkpoint.sequence_index)?
                .ok_or_else(|| {
                    anyhow!(
                        "checkpoint {} is missing previous checkpoint state",
                        checkpoint.checkpoint_id
                    )
                })?;
        previous.accepted_commit_sha.ok_or_else(|| {
            anyhow!(
                "previous checkpoint {} must be accepted before submitting a candidate for checkpoint {}",
                previous.checkpoint_id,
                checkpoint.checkpoint_id
            )
        })?
    };
    if let Some(later_checkpoint) = query::load_active_checkpoint_state(transaction, loop_id)?
        .into_iter()
        .find(|candidate| {
            candidate.sequence_index > checkpoint.sequence_index
                && candidate.accepted_commit_sha.is_some()
        })
    {
        bail!(
            "later checkpoint {} is already accepted; checkpoint {} cannot submit a new candidate after it",
            later_checkpoint.checkpoint_id,
            checkpoint.checkpoint_id
        );
    }

    system::git_verify(
        &runtime.workspace_root,
        &[
            git_dir_arg.as_str(),
            "cat-file",
            "-e",
            &format!("{candidate_commit_sha}^{{commit}}"),
        ],
    )
    .with_context(|| format!("candidate commit {} does not exist", candidate_commit_sha))?;
    system::git_verify(
        &runtime.workspace_root,
        &[
            git_dir_arg.as_str(),
            "merge-base",
            "--is-ancestor",
            candidate_commit_sha,
            &loop_state.worktree_branch,
        ],
    )
    .with_context(|| {
        format!(
            "candidate commit {} is not reachable from branch {}",
            candidate_commit_sha, loop_state.worktree_branch
        )
    })?;
    system::git_verify(
        &runtime.workspace_root,
        &[
            git_dir_arg.as_str(),
            "merge-base",
            "--is-ancestor",
            &expected_base,
            candidate_commit_sha,
        ],
    )
    .with_context(|| {
        format!(
            "candidate commit {} is not a descendant of expected base {}",
            candidate_commit_sha, expected_base
        )
    })?;
    Ok(())
}
