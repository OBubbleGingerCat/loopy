// Invocation ops own invocation opening and dispatch workflows; they do not own projection replay.

use super::super::{projection, query, roles, system, *};
use super::r#loop::append_failure_result;

const MAX_DISPATCH_ATTEMPTS: usize = 5;
const MAX_TIMEOUT_RETRY_MULTIPLIER: i64 = 4;

struct OpenedInvocation {
    invocation_id: String,
    token: String,
    role_definition_ref: String,
    executor_config_ref: String,
    invocation_context_ref: String,
}

struct DispatchExecution {
    command: Vec<String>,
    cwd: String,
    dispatch_envelope: String,
    env_policy: String,
    env_allow: Vec<String>,
    timeout_sec: i64,
}

struct DispatchOutcome {
    accepted_terminal_api: Option<String>,
    terminal_result: Option<Value>,
    transcript_segment_count: usize,
}

struct TimeoutRetryDecision {
    next_timeout_sec: i64,
    latest_request: TimeoutExtensionRequestState,
}

fn load_bypass_sandbox(loop_input: &Value) -> bool {
    loop_input
        .get("bypass_sandbox")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn open_worker_invocation(
    runtime: &Runtime,
    request: StartWorkerInvocationRequest,
) -> Result<OpenedInvocation> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;

    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    query::ensure_loop_status_is_open(&loop_state, "open worker invocations")?;
    if query::load_caller_finalize_status(&transaction, &request.loop_id)?.is_some() {
        bail!("cannot open worker invocations after caller finalize handoff");
    }
    if matches!(request.stage, WorkerStage::Planning) && loop_state.phase != "planning" {
        bail!(
            "planning worker invocations require prepare-worktree to succeed first; current phase is {}",
            loop_state.phase
        );
    }
    let bound_checkpoint = if matches!(request.stage, WorkerStage::Artifact) {
        let checkpoint_id = request
            .checkpoint_id
            .as_deref()
            .ok_or_else(|| anyhow!("artifact worker invocations require checkpoint_id"))?;
        Some(query::load_checkpoint_state(
            &transaction,
            &request.loop_id,
            checkpoint_id,
        )?)
    } else {
        None
    };
    let skill_root = runtime.installed_skill_root();
    let manifest = roles::load_manifest(&skill_root)?;
    let resolved_role_selection = query::load_resolved_role_selection(
        &transaction,
        &skill_root,
        &loop_state.resolved_role_selection_ref,
    )?;
    let (role_kind, selected_role_id) = match request.stage {
        WorkerStage::Planning => ("planning_worker", resolved_role_selection.planning_worker),
        WorkerStage::Artifact => ("artifact_worker", resolved_role_selection.artifact_worker),
    };
    let (role_path, role_markdown, role_front_matter, executor_profile) =
        roles::load_task_type_role_definition(
            &skill_root,
            &manifest,
            &resolved_role_selection.task_type,
            role_kind,
            &selected_role_id,
        )?;

    let invocation_id = format!("inv-{}", Uuid::now_v7());
    let token = format!("tok-{}", Uuid::now_v7());
    let invocation_context_path = runtime
        .workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"));
    let allowed_terminal_apis = request
        .stage
        .allowed_terminal_apis()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let loop_input = query::load_json_content(&transaction, &loop_state.loop_input_ref)?;
    let bypass_sandbox = load_bypass_sandbox(&loop_input);
    let (command, args_variant) = roles::resolve_executor_command(
        &executor_profile,
        &skill_root,
        &runtime.workspace_root,
        &loop_state.worktree_path,
        &invocation_context_path,
        bypass_sandbox,
    )?;
    let env_policy = roles::resolve_executor_env_policy(&executor_profile, bypass_sandbox);
    let env_allow = executor_profile.env_allow.clone().unwrap_or_default();

    let role_definition_ref = projection::store_json_content(
        &transaction,
        "role_definition",
        &json!({
            "role": role_front_matter.role,
            "executor": role_front_matter.executor,
            "prompt_markdown": role_markdown,
        }),
    )?;
    let resolved_executor_config = json!({
        "kind": executor_profile.kind,
        "command": command,
        "cwd": roles::resolve_executor_cwd(
            &executor_profile.cwd,
            &runtime.workspace_root,
            &loop_state.worktree_path,
        ),
        "timeout_sec": executor_profile.timeout_sec,
        "transcript_capture": executor_profile.transcript_capture,
        "bypass_sandbox": bypass_sandbox,
        "args_variant": args_variant,
        "env_policy": env_policy,
        "env_allow": env_allow,
    });
    let executor_config_ref = projection::store_json_content(
        &transaction,
        "resolved_executor_config",
        &resolved_executor_config,
    )?;
    let review_history = query::build_worker_review_history_payload(
        &transaction,
        &request.loop_id,
        &loop_state,
        &request.stage,
        request.checkpoint_id.as_deref(),
    )?;
    let review_history_ref =
        projection::store_json_content(&transaction, "review_history", &review_history)?;
    let invocation_context_payload = json!({
        "loop_id": request.loop_id,
        "invocation_id": invocation_id,
        "token": token,
        "actor_role": "worker",
        "stage": request.stage.as_str(),
        "checkpoint_id": request.checkpoint_id,
        "bound_checkpoint": bound_checkpoint.as_ref().map(query::build_checkpoint_payload),
        "allowed_terminal_apis": allowed_terminal_apis,
        "role_definition_ref": role_definition_ref,
        "resolved_executor_config_ref": executor_config_ref,
        "loopy_api_contract": roles::build_loopy_api_contract(&allowed_terminal_apis),
        "bundle_root": skill_root,
        "bundle_bin": skill_root.join("bin/loopy-submit-loop"),
        "task_type": resolved_role_selection.task_type,
        "loop_input_ref": loop_state.loop_input_ref,
        "loop_input": loop_input,
        "review_history_ref": review_history_ref,
        "review_history": review_history,
        "selected_role_id": selected_role_id,
        "selected_role_path": role_path.display().to_string(),
        "worktree_ref": {
            "path": loop_state.worktree_path,
            "branch": loop_state.worktree_branch,
            "label": loop_state.worktree_label,
        },
        "invocation_context_path": invocation_context_path,
    });
    let invocation_context_ref = projection::store_json_content(
        &transaction,
        "invocation_context",
        &invocation_context_payload,
    )?;

    projection::append_event(
        &transaction,
        &request.loop_id,
        "CORE__invocation_opened",
        &json!({
            "invocation_id": invocation_id,
            "invocation_role": "worker",
            "stage": request.stage.as_str(),
            "status": "opened",
            "token": token,
            "allowed_terminal_apis": allowed_terminal_apis,
            "role_definition_ref": role_definition_ref,
            "resolved_executor_config_ref": executor_config_ref,
            "invocation_context_ref": invocation_context_ref,
            "review_round_id": Value::Null,
            "review_slot_id": Value::Null,
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;
    // Materialize the on-disk context only after the durable event commit succeeds.
    system::write_invocation_context_file(&invocation_context_payload)?;

    Ok(OpenedInvocation {
        invocation_id,
        token,
        role_definition_ref,
        executor_config_ref,
        invocation_context_ref,
    })
}

pub(crate) fn start_worker_invocation(
    runtime: &Runtime,
    request: StartWorkerInvocationRequest,
) -> Result<StartWorkerInvocationResponse> {
    let opened = open_worker_invocation(runtime, request)?;
    let dispatch = dispatch_invocation(runtime, &opened.invocation_id)?;
    Ok(StartInvocationResponse {
        invocation_id: opened.invocation_id,
        token: opened.token,
        role_definition_ref: opened.role_definition_ref,
        executor_config_ref: opened.executor_config_ref,
        invocation_context_ref: opened.invocation_context_ref,
        accepted_terminal_api: dispatch.accepted_terminal_api,
        terminal_result: dispatch.terminal_result,
        transcript_segment_count: dispatch.transcript_segment_count,
    })
}

fn open_reviewer_invocation(
    runtime: &Runtime,
    request: StartReviewerInvocationRequest,
) -> Result<OpenedInvocation> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;

    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    query::ensure_loop_status_is_open(&loop_state, "open reviewer invocations")?;
    if query::load_caller_finalize_status(&transaction, &request.loop_id)?.is_some() {
        bail!("cannot start reviewer invocations after caller finalize handoff");
    }
    let review_round_state =
        query::load_review_round_state(&transaction, &request.loop_id, &request.review_round_id)?;
    let Some(slot_state) = review_round_state
        .slot_state
        .iter()
        .find(|slot| slot.review_slot_id == request.review_slot_id && slot.status == "pending")
        .cloned()
    else {
        bail!(
            "review slot {} is not pending in review round {}",
            request.review_slot_id,
            request.review_round_id
        );
    };

    let skill_root = runtime.installed_skill_root();
    let manifest = roles::load_manifest(&skill_root)?;
    let review_kind = match review_round_state.review_kind.as_str() {
        "checkpoint" => ReviewKind::Checkpoint,
        "artifact" => ReviewKind::Artifact,
        other => bail!("unsupported review kind {other}"),
    };
    let resolved_role_selection = query::load_resolved_role_selection(
        &transaction,
        &skill_root,
        &loop_state.resolved_role_selection_ref,
    )?;
    let reviewer_role_id = slot_state.reviewer_role_id.clone().ok_or_else(|| {
        anyhow!(
            "review slot {} is missing reviewer_role_id; reopen the review round",
            request.review_slot_id
        )
    })?;
    let expected_reviewer_ids = match review_kind {
        ReviewKind::Checkpoint => &resolved_role_selection.checkpoint_reviewers,
        ReviewKind::Artifact => &resolved_role_selection.artifact_reviewers,
    };
    if !expected_reviewer_ids
        .iter()
        .any(|role_id| role_id == &reviewer_role_id)
    {
        bail!(
            "review slot {} bound reviewer role {} is not in loop reviewer selection",
            request.review_slot_id,
            reviewer_role_id
        );
    }
    let role_kind = match review_kind {
        ReviewKind::Checkpoint => "checkpoint_reviewer",
        ReviewKind::Artifact => "artifact_reviewer",
    };
    let (role_path, role_markdown, role_front_matter, executor_profile) =
        roles::load_task_type_role_definition(
            &skill_root,
            &manifest,
            &resolved_role_selection.task_type,
            role_kind,
            &reviewer_role_id,
        )?;

    let invocation_id = format!("inv-{}", Uuid::now_v7());
    let token = format!("tok-{}", Uuid::now_v7());
    let invocation_context_path = runtime
        .workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"));
    let allowed_terminal_apis = review_kind
        .allowed_terminal_apis()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let loop_input = query::load_json_content(&transaction, &loop_state.loop_input_ref)?;
    let bypass_sandbox = load_bypass_sandbox(&loop_input);
    let (command, args_variant) = roles::resolve_executor_command(
        &executor_profile,
        &skill_root,
        &runtime.workspace_root,
        &loop_state.worktree_path,
        &invocation_context_path,
        bypass_sandbox,
    )?;
    let env_policy = roles::resolve_executor_env_policy(&executor_profile, bypass_sandbox);
    let env_allow = executor_profile.env_allow.clone().unwrap_or_default();

    let role_definition_ref = projection::store_json_content(
        &transaction,
        "role_definition",
        &json!({
            "role": role_front_matter.role,
            "executor": role_front_matter.executor,
            "prompt_markdown": role_markdown,
        }),
    )?;
    let resolved_executor_config = json!({
        "kind": executor_profile.kind,
        "command": command,
        "cwd": roles::resolve_executor_cwd(
            &executor_profile.cwd,
            &runtime.workspace_root,
            &loop_state.worktree_path,
        ),
        "timeout_sec": executor_profile.timeout_sec,
        "transcript_capture": executor_profile.transcript_capture,
        "bypass_sandbox": bypass_sandbox,
        "args_variant": args_variant,
        "env_policy": env_policy,
        "env_allow": env_allow,
    });
    let executor_config_ref = projection::store_json_content(
        &transaction,
        "resolved_executor_config",
        &resolved_executor_config,
    )?;
    let reviewer_history = query::build_reviewer_review_history_payload(
        &transaction,
        &request.loop_id,
        review_kind.as_str(),
        &reviewer_role_id,
    )?;
    let reviewer_history_ref =
        projection::store_json_content(&transaction, "reviewer_history", &reviewer_history)?;
    let invocation_context_payload = json!({
        "loop_id": request.loop_id,
        "invocation_id": invocation_id,
        "token": token,
        "actor_role": "reviewer",
        "stage": review_kind.stage(),
        "review_round_id": request.review_round_id,
        "review_slot_id": request.review_slot_id,
        "review_kind": review_kind.as_str(),
        "review_target": query::build_review_target_payload(&review_round_state),
        "allowed_terminal_apis": allowed_terminal_apis,
        "role_definition_ref": role_definition_ref,
        "resolved_executor_config_ref": executor_config_ref,
        "loopy_api_contract": roles::build_loopy_api_contract(&allowed_terminal_apis),
        "bundle_root": skill_root,
        "bundle_bin": skill_root.join("bin/loopy-submit-loop"),
        "task_type": resolved_role_selection.task_type,
        "loop_input_ref": loop_state.loop_input_ref,
        "loop_input": loop_input,
        "reviewer_history_ref": reviewer_history_ref,
        "reviewer_history": reviewer_history,
        "reviewer_role_id": reviewer_role_id,
        "selected_role_id": reviewer_role_id,
        "selected_role_path": role_path.display().to_string(),
        "worktree_ref": {
            "path": loop_state.worktree_path,
            "branch": loop_state.worktree_branch,
            "label": loop_state.worktree_label,
        },
        "invocation_context_path": invocation_context_path,
    });
    let invocation_context_ref = projection::store_json_content(
        &transaction,
        "invocation_context",
        &invocation_context_payload,
    )?;

    projection::append_event(
        &transaction,
        &request.loop_id,
        "CORE__invocation_opened",
        &json!({
            "invocation_id": invocation_id,
            "invocation_role": "reviewer",
            "stage": review_kind.stage(),
            "status": "opened",
            "token": token,
            "allowed_terminal_apis": allowed_terminal_apis,
            "role_definition_ref": role_definition_ref,
            "resolved_executor_config_ref": executor_config_ref,
            "invocation_context_ref": invocation_context_ref,
            "review_round_id": request.review_round_id,
            "review_slot_id": request.review_slot_id,
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;
    system::write_invocation_context_file(&invocation_context_payload)?;

    Ok(OpenedInvocation {
        invocation_id,
        token,
        role_definition_ref,
        executor_config_ref,
        invocation_context_ref,
    })
}

pub(crate) fn start_reviewer_invocation(
    runtime: &Runtime,
    request: StartReviewerInvocationRequest,
) -> Result<StartReviewerInvocationResponse> {
    let opened = open_reviewer_invocation(runtime, request)?;
    let dispatch = dispatch_invocation(runtime, &opened.invocation_id)?;
    Ok(StartInvocationResponse {
        invocation_id: opened.invocation_id,
        token: opened.token,
        role_definition_ref: opened.role_definition_ref,
        executor_config_ref: opened.executor_config_ref,
        invocation_context_ref: opened.invocation_context_ref,
        accepted_terminal_api: dispatch.accepted_terminal_api,
        terminal_result: dispatch.terminal_result,
        transcript_segment_count: dispatch.transcript_segment_count,
    })
}

fn dispatch_invocation(runtime: &Runtime, invocation_id: &str) -> Result<DispatchOutcome> {
    let mut connection = runtime.open_connection()?;

    let dispatch_state = {
        let transaction = begin_immediate_transaction(&mut connection)?;
        let dispatch_state = query::load_invocation_dispatch_state(&transaction, invocation_id)?;
        let invocation_context_payload =
            query::load_json_content(&transaction, &dispatch_state.invocation_context_ref)?;
        system::write_invocation_context_file(&invocation_context_payload)?;
        projection::append_event(
            &transaction,
            &dispatch_state.loop_id,
            "CORE__request_materialized",
            &json!({
                "invocation_id": invocation_id,
                "status": "request_materialized",
            }),
        )?;
        projection::rebuild_all_projections(&transaction)?;
        transaction.commit()?;
        dispatch_state
    };

    let mut current_timeout_sec =
        load_dispatch_execution(&mut connection, &dispatch_state)?.timeout_sec;
    for attempt in 1..=MAX_DISPATCH_ATTEMPTS {
        let transaction = begin_immediate_transaction(&mut connection)?;
        projection::append_event(
            &transaction,
            &dispatch_state.loop_id,
            "CORE__dispatch_started",
            &json!({
                "invocation_id": invocation_id,
                "status": "dispatch_started",
                "attempt": attempt,
                "max_attempts": MAX_DISPATCH_ATTEMPTS,
                "timeout_sec": current_timeout_sec,
            }),
        )?;
        projection::rebuild_all_projections(&transaction)?;
        transaction.commit()?;
        let execution = load_dispatch_execution(&mut connection, &dispatch_state)?;

        match system::run_local_command(
            &execution.command,
            Path::new(&execution.cwd),
            Some(&execution.dispatch_envelope),
            current_timeout_sec,
            &execution.env_policy,
            &execution.env_allow,
        ) {
            Ok(output) => {
                let (accepted_terminal_api, terminal_result, transcript_segment_count) =
                    record_dispatch_output(
                        &mut connection,
                        &dispatch_state,
                        invocation_id,
                        output,
                    )?;
                return Ok(DispatchOutcome {
                    accepted_terminal_api,
                    terminal_result,
                    transcript_segment_count,
                });
            }
            Err(error) => {
                let failure_reason = format!("{error:#}");
                let failure_kind = if failure_reason.contains("timed out") {
                    "timeout"
                } else {
                    "launch_error"
                };
                let retry_decision = if failure_kind == "timeout" && attempt < MAX_DISPATCH_ATTEMPTS
                {
                    let transaction = begin_immediate_transaction(&mut connection)?;
                    let retry = maybe_select_timeout_retry(
                        &transaction,
                        invocation_id,
                        current_timeout_sec,
                    )?;
                    transaction.commit()?;
                    retry
                } else {
                    None
                };
                let transaction = begin_immediate_transaction(&mut connection)?;
                projection::append_event(
                    &transaction,
                    &dispatch_state.loop_id,
                    "CORE__invocation_failed",
                    &json!({
                        "invocation_id": invocation_id,
                        "status": "failed",
                        "failure_kind": failure_kind,
                        "reason": failure_reason,
                        "attempt": attempt,
                        "max_attempts": MAX_DISPATCH_ATTEMPTS,
                        "timeout_sec": current_timeout_sec,
                    }),
                )?;
                if let Some(retry) = retry_decision {
                    projection::append_event(
                        &transaction,
                        &dispatch_state.loop_id,
                        "CORE__dispatch_retry_scheduled",
                        &json!({
                            "invocation_id": invocation_id,
                            "attempt": attempt,
                            "next_attempt": attempt + 1,
                            "previous_timeout_sec": current_timeout_sec,
                            "next_timeout_sec": retry.next_timeout_sec,
                            "requested_timeout_sec": retry.latest_request.requested_timeout_sec,
                            "request_content_ref": retry.latest_request.request_content_ref,
                            "progress_summary": retry.latest_request.progress_summary,
                            "rationale": retry.latest_request.rationale,
                        }),
                    )?;
                    projection::rebuild_all_projections(&transaction)?;
                    transaction.commit()?;
                    current_timeout_sec = retry.next_timeout_sec;
                    continue;
                }
                if failure_kind == "timeout" && attempt == MAX_DISPATCH_ATTEMPTS {
                    let latest_request =
                        query::load_latest_timeout_extension_request(&transaction, invocation_id)?;
                    append_dispatch_retry_budget_failure(
                        &transaction,
                        &dispatch_state.loop_id,
                        invocation_id,
                        current_timeout_sec,
                        latest_request.as_ref(),
                    )?;
                    projection::rebuild_all_projections(&transaction)?;
                    transaction.commit()?;
                    return Err(error);
                }
                projection::rebuild_all_projections(&transaction)?;
                transaction.commit()?;
                return Err(error);
            }
        }
    }

    bail!(
        "invocation {} exhausted the dispatch retry budget",
        invocation_id
    )
}

fn append_dispatch_retry_budget_failure(
    transaction: &Transaction<'_>,
    loop_id: &str,
    invocation_id: &str,
    current_timeout_sec: i64,
    latest_request: Option<&TimeoutExtensionRequestState>,
) -> Result<()> {
    let loop_state = query::load_loop_state(transaction, loop_id)?;
    let mut last_stable_context = json!({
        "invocation_id": invocation_id,
        "last_timeout_sec": current_timeout_sec,
        "max_attempts": MAX_DISPATCH_ATTEMPTS,
    });
    if let (Some(context), Some(request)) = (last_stable_context.as_object_mut(), latest_request) {
        context.insert(
            "latest_timeout_extension_request".to_owned(),
            json!({
                "request_content_ref": request.request_content_ref,
                "requested_timeout_sec": request.requested_timeout_sec,
                "progress_summary": request.progress_summary,
                "rationale": request.rationale,
            }),
        );
    }
    let summary = format!(
        "invocation {invocation_id} exhausted the dispatch retry budget after {MAX_DISPATCH_ATTEMPTS} timed-out attempts"
    );
    append_failure_result(
        transaction,
        loop_id,
        &loop_state,
        "system_failure",
        &summary,
        &loop_state.phase,
        &last_stable_context,
    )?;
    Ok(())
}

fn load_dispatch_execution(
    connection: &mut Connection,
    dispatch_state: &InvocationDispatchState,
) -> Result<DispatchExecution> {
    let transaction = begin_immediate_transaction(connection)?;
    let executor_config =
        query::load_json_content(&transaction, &dispatch_state.executor_config_ref)?;
    let invocation_context =
        query::load_json_content(&transaction, &dispatch_state.invocation_context_ref)?;
    let role_definition_ref = required_str(&invocation_context, "role_definition_ref")?;
    let role_definition = query::load_json_content(&transaction, role_definition_ref)?;
    let dispatch_envelope = roles::build_dispatch_envelope(&role_definition, &invocation_context)?;
    let command = executor_config
        .get("command")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("resolved executor config missing command array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("executor command element must be a string"))
        })
        .collect::<Result<Vec<_>>>()?;
    let cwd = executor_config
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("resolved executor config missing cwd"))?
        .to_owned();
    let timeout_sec = executor_config
        .get("timeout_sec")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("resolved executor config missing timeout_sec"))?;
    let env_policy = match executor_config.get("env_policy") {
        None => "allowlist",
        Some(Value::String(policy)) => policy.as_str(),
        Some(_) => bail!("resolved executor config env_policy must be a string"),
    };
    let env_allow = executor_config
        .get("env_allow")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("executor env_allow entries must be strings"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    transaction.commit()?;
    Ok(DispatchExecution {
        command,
        cwd,
        dispatch_envelope,
        env_policy: env_policy.to_owned(),
        env_allow,
        timeout_sec,
    })
}

fn maybe_select_timeout_retry(
    transaction: &Transaction<'_>,
    invocation_id: &str,
    current_timeout_sec: i64,
) -> Result<Option<TimeoutRetryDecision>> {
    let Some(latest_request) =
        query::load_latest_timeout_extension_request(transaction, invocation_id)?
    else {
        return Ok(None);
    };
    if latest_request.requested_timeout_sec <= current_timeout_sec {
        return Ok(None);
    }
    if !timeout_request_has_progress_evidence(&latest_request) {
        return Ok(None);
    }
    if !timeout_request_is_proportionate(latest_request.requested_timeout_sec, current_timeout_sec)
    {
        return Ok(None);
    }
    Ok(Some(TimeoutRetryDecision {
        next_timeout_sec: latest_request.requested_timeout_sec,
        latest_request,
    }))
}

fn timeout_request_has_progress_evidence(request: &TimeoutExtensionRequestState) -> bool {
    super::timeout_request_has_progress_evidence(&request.progress_summary, &request.rationale)
}

fn timeout_request_is_proportionate(requested_timeout_sec: i64, current_timeout_sec: i64) -> bool {
    requested_timeout_sec <= current_timeout_sec.saturating_mul(MAX_TIMEOUT_RETRY_MULTIPLIER)
}

fn record_dispatch_output(
    connection: &mut Connection,
    dispatch_state: &InvocationDispatchState,
    invocation_id: &str,
    output: std::process::Output,
) -> Result<(Option<String>, Option<Value>, usize)> {
    let transaction = begin_immediate_transaction(connection)?;
    let stdout_ref = projection::store_json_content(
        &transaction,
        "transcript_segment",
        &json!({
            "stream": "stdout",
            "text": String::from_utf8_lossy(&output.stdout),
        }),
    )?;
    projection::append_transcript_segment(
        &transaction,
        invocation_id,
        "executor",
        "free_text_output",
        Some("executor stdout"),
        &stdout_ref,
    )?;
    if !output.stderr.is_empty() {
        let stderr_ref = projection::store_json_content(
            &transaction,
            "transcript_segment",
            &json!({
                "stream": "stderr",
                "text": String::from_utf8_lossy(&output.stderr),
            }),
        )?;
        projection::append_transcript_segment(
            &transaction,
            invocation_id,
            "executor",
            "free_text_output",
            Some("executor stderr"),
            &stderr_ref,
        )?;
    }
    projection::append_event(
        &transaction,
        &dispatch_state.loop_id,
        "CORE__response_received",
        &json!({
            "invocation_id": invocation_id,
            "status": "response_received",
            "exit_code": output.status.code(),
        }),
    )?;
    let accepted_terminal_api = transaction
        .query_row(
            "SELECT accepted_api FROM CORE__invocation_current WHERE invocation_id = ?1",
            [invocation_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let terminal_result = if accepted_terminal_api.is_some() {
        query::load_existing_result_ref(&transaction, &dispatch_state.loop_id)?
            .map(|result_ref| query::load_json_content(&transaction, &result_ref))
            .transpose()?
    } else {
        None
    };
    if accepted_terminal_api.is_none() {
        projection::append_event(
            &transaction,
            &dispatch_state.loop_id,
            "CORE__invocation_failed",
            &json!({
                "invocation_id": invocation_id,
                "status": "failed",
                "failure_kind": "protocol_error",
                "reason": "no terminal API call was accepted",
            }),
        )?;
    }
    projection::rebuild_all_projections(&transaction)?;
    let transcript_segment_count = transaction.query_row(
        "SELECT COUNT(*) FROM CORE__transcript_segments WHERE invocation_id = ?1",
        [invocation_id],
        |row| row.get::<_, i64>(0),
    )? as usize;
    transaction.commit()?;
    Ok((
        accepted_terminal_api,
        terminal_result,
        transcript_segment_count,
    ))
}
