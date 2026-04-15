// Caller-finalize ops own the handoff boundary between coordinator acceptance and caller-owned integration.

use super::super::{projection, query, *};
use super::r#loop::{
    WorktreeCleanupOutcome, append_worktree_cleanup_event, cleanup_disposable_worktree,
    load_existing_result_payload,
};

fn build_caller_finalize_response(
    transaction: &Transaction<'_>,
    loop_id: &str,
    loop_state: &LoopState,
) -> Result<HandoffToCallerFinalizeResponse> {
    let task_summary = query::load_loop_task_summary(transaction, &loop_state.loop_input_ref)?;
    let active_checkpoints = query::load_active_checkpoint_state(transaction, loop_id)?;
    let artifact_summary = active_checkpoints
        .iter()
        .map(|checkpoint| {
            let accepted_commit_sha = checkpoint
                .accepted_commit_sha
                .clone()
                .ok_or_else(|| anyhow!("accepted checkpoint missing accepted commit sha"))?;
            let artifact = query::load_accepted_artifact_material(
                transaction,
                loop_id,
                &checkpoint.checkpoint_id,
                &accepted_commit_sha,
            )?;
            Ok(CallerFinalizeArtifactSummary {
                checkpoint_id: checkpoint.checkpoint_id.clone(),
                checkpoint_title: artifact.title,
                accepted_commit_sha,
                change_summary: artifact.change_summary,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let improvement_opportunities =
        query::load_caller_facing_improvement_summaries(transaction, loop_id)?;

    Ok(HandoffToCallerFinalizeResponse {
        loop_id: loop_id.to_owned(),
        phase: loop_state.phase.clone(),
        task_summary,
        worktree_ref: CallerFinalizeWorktreeRef {
            path: loop_state.worktree_path.clone(),
            branch: loop_state.worktree_branch.clone(),
            label: loop_state.worktree_label.clone(),
        },
        artifact_summary,
        improvement_opportunities,
    })
}

pub(crate) fn handoff_to_caller_finalize(
    runtime: &Runtime,
    request: HandoffToCallerFinalizeRequest,
) -> Result<HandoffToCallerFinalizeResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    query::ensure_loop_status_is_open(&loop_state, "handoff to caller finalize")?;
    if let Some(caller_finalize_status) =
        query::load_caller_finalize_status(&transaction, &request.loop_id)?
    {
        match caller_finalize_status.as_str() {
            "ready" => {
                let response =
                    build_caller_finalize_response(&transaction, &request.loop_id, &loop_state)?;
                transaction.commit()?;
                return Ok(response);
            }
            "active" | "blocked" => {
                bail!("cannot hand off caller finalize after caller finalize has already started");
            }
            _ => {
                bail!(
                    "cannot hand off caller finalize from caller finalize status {caller_finalize_status}"
                );
            }
        }
    }
    if query::has_integrated_commits_event(&transaction, &request.loop_id)? {
        bail!(
            "cannot hand off caller finalize after accepted commits were already integrated into the caller branch"
        );
    }
    let active_checkpoints = query::load_active_checkpoint_state(&transaction, &request.loop_id)?;
    if active_checkpoints.is_empty() {
        bail!("cannot hand off caller finalize before accepted artifact state exists");
    }
    if active_checkpoints.iter().any(|checkpoint| {
        checkpoint.execution_state != "accepted" || checkpoint.accepted_commit_sha.is_none()
    }) {
        bail!(
            "cannot hand off caller finalize before every active checkpoint in the executable plan is accepted"
        );
    }

    projection::append_event(
        &transaction,
        &request.loop_id,
        "SUBMIT_LOOP__caller_finalize_handed_off",
        &json!({
            "phase": "ready_for_caller_finalize",
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    let ready_loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    let response =
        build_caller_finalize_response(&transaction, &request.loop_id, &ready_loop_state)?;
    transaction.commit()?;
    Ok(response)
}

pub(crate) fn begin_caller_finalize(
    runtime: &Runtime,
    request: BeginCallerFinalizeRequest,
) -> Result<BeginCallerFinalizeResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    query::ensure_loop_status_is_open(&loop_state, "begin caller finalize")?;
    let caller_finalize_status =
        query::load_caller_finalize_status(&transaction, &request.loop_id)?
            .ok_or_else(|| anyhow!("cannot begin caller finalize before coordinator handoff"))?;
    if caller_finalize_status == "active" {
        let response = build_caller_finalize_response(&transaction, &request.loop_id, &loop_state)?;
        transaction.commit()?;
        return Ok(BeginCallerFinalizeResponse {
            loop_id: response.loop_id,
            phase: "caller_finalizing".to_owned(),
            task_summary: response.task_summary,
            worktree_ref: response.worktree_ref,
            artifact_summary: response.artifact_summary,
            improvement_opportunities: response.improvement_opportunities,
        });
    }
    if caller_finalize_status != "ready" && caller_finalize_status != "blocked" {
        bail!("cannot begin caller finalize from caller finalize status {caller_finalize_status}");
    }

    projection::append_event(
        &transaction,
        &request.loop_id,
        "SUBMIT_LOOP__caller_finalize_started",
        &json!({
            "phase": "caller_finalizing",
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    let active_loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    let response =
        build_caller_finalize_response(&transaction, &request.loop_id, &active_loop_state)?;
    transaction.commit()?;
    Ok(BeginCallerFinalizeResponse {
        loop_id: response.loop_id,
        phase: "caller_finalizing".to_owned(),
        task_summary: response.task_summary,
        worktree_ref: response.worktree_ref,
        artifact_summary: response.artifact_summary,
        improvement_opportunities: response.improvement_opportunities,
    })
}

pub(crate) fn block_caller_finalize(
    runtime: &Runtime,
    request: BlockCallerFinalizeRequest,
) -> Result<BlockCallerFinalizeResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    query::ensure_loop_status_is_open(&loop_state, "block caller finalize")?;
    let caller_finalize_status =
        query::load_caller_finalize_status(&transaction, &request.loop_id)?
            .ok_or_else(|| anyhow!("cannot block caller finalize before coordinator handoff"))?;
    if caller_finalize_status != "active" || loop_state.phase != "caller_finalizing" {
        bail!(
            "cannot block caller finalize from loop phase {}",
            loop_state.phase
        );
    }
    let block_context_ref = projection::store_json_content(
        &transaction,
        "caller_finalize_block_context",
        &json!({
            "caller_branch": system::git_current_branch(runtime.workspace_root())?,
            "caller_head_sha": system::git_head_sha(runtime.workspace_root())?,
            "task_summary": query::load_loop_task_summary(&transaction, &loop_state.loop_input_ref)?,
            "worktree_ref": {
                "path": loop_state.worktree_path.clone(),
                "branch": loop_state.worktree_branch.clone(),
                "label": loop_state.worktree_label.clone(),
            },
            "strategy_summary": request.strategy_summary,
            "blocking_summary": request.blocking_summary,
            "human_question": request.human_question,
            "conflicting_files": request.conflicting_files,
            "notes": request.notes,
            "has_in_progress_integration": request.has_in_progress_integration,
        }),
    )?;
    projection::append_event(
        &transaction,
        &request.loop_id,
        "SUBMIT_LOOP__caller_finalize_blocked",
        &json!({
            "phase": "caller_blocked_on_human",
            "block_context_ref": block_context_ref,
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;
    Ok(BlockCallerFinalizeResponse {
        loop_id: request.loop_id,
        phase: "caller_blocked_on_human".to_owned(),
        status: "blocked".to_owned(),
    })
}

pub(crate) fn finalize_success(
    runtime: &Runtime,
    request: FinalizeSuccessRequest,
) -> Result<FinalizeSuccessResponse> {
    if let Some(existing_result) = load_existing_result_payload(runtime, &request.loop_id)? {
        match existing_result.get("status").and_then(Value::as_str) {
            Some("success") => return Ok(existing_result),
            Some("failure") => {
                bail!(
                    "cannot finalize success after a failure result has already been materialized"
                )
            }
            Some(status) => {
                bail!("cannot finalize success with existing result status {status}")
            }
            None => {
                bail!(
                    "cannot finalize success because the existing result payload is missing a status"
                )
            }
        }
    }

    let caller_branch = system::git_current_branch(runtime.workspace_root())?;
    let final_head_sha = system::git_head_sha(runtime.workspace_root())?;
    let (loop_state, worktree_lifecycle) = {
        let mut connection = runtime.open_connection()?;
        let transaction = begin_immediate_transaction(&mut connection)?;
        let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
        query::ensure_loop_status_is_open(&loop_state, "finalize success")?;
        ensure_success_result_ready_for_caller_finalize(&transaction, &request.loop_id)?;
        validate_caller_integration_summary(
            &transaction,
            runtime,
            &request.loop_id,
            &request.integration_summary,
            &final_head_sha,
        )?;
        let worktree_lifecycle = query::load_worktree_lifecycle(&transaction, &request.loop_id)?;
        transaction.commit()?;
        (loop_state, worktree_lifecycle)
    };

    let cleanup_outcome = match worktree_lifecycle.as_deref() {
        Some("deleted") | Some("cleanup_warning") => None,
        _ => Some(
            cleanup_disposable_worktree(
                runtime,
                Path::new(&loop_state.worktree_path),
                &loop_state.worktree_label,
            )
            .map(|_| WorktreeCleanupOutcome::Deleted)
            .unwrap_or_else(|_| WorktreeCleanupOutcome::Warning {
                summary: format!(
                    "failed to remove disposable worktree {} after cleanup retries",
                    loop_state.worktree_path
                ),
            }),
        ),
    };

    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let active_loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    query::ensure_loop_status_is_open(&active_loop_state, "finalize success")?;
    ensure_success_result_ready_for_caller_finalize(&transaction, &request.loop_id)?;
    projection::append_event(
        &transaction,
        &request.loop_id,
        "SUBMIT_LOOP__caller_integration_recorded",
        &json!({
            "caller_branch": caller_branch,
            "final_head_sha": final_head_sha,
            "strategy": request.integration_summary.strategy,
            "landed_commit_shas": request.integration_summary.landed_commit_shas,
            "resolution_notes": request.integration_summary.resolution_notes,
        }),
    )?;
    if let Some(cleanup_outcome) = cleanup_outcome.as_ref() {
        append_worktree_cleanup_event(
            &transaction,
            &request.loop_id,
            &active_loop_state,
            cleanup_outcome,
        )?;
        // Success materialization reads the projected worktree lifecycle.
        projection::rebuild_all_projections(&transaction)?;
    }
    let result = materialize_success_result(&transaction, &request.loop_id)?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;
    Ok(result)
}

fn materialize_success_result(transaction: &Transaction<'_>, loop_id: &str) -> Result<Value> {
    if let Some(existing_result_ref) = query::load_existing_result_ref(transaction, loop_id)? {
        return query::load_json_content(transaction, &existing_result_ref);
    }

    let worktree_lifecycle = query::load_worktree_lifecycle(transaction, loop_id)?;
    let cleanup_warning = match worktree_lifecycle.as_deref() {
        Some("deleted") => None,
        Some("cleanup_warning") => Some(
            query::load_latest_worktree_cleanup_warning_event(transaction, loop_id)?
                .ok_or_else(|| {
                    anyhow!(
                        "cannot materialize success result with cleanup_warning lifecycle before warning material is recorded"
                    )
                })?,
        ),
        _ => {
            bail!(
                "cannot materialize success result before worktree cleanup is recorded or downgraded to a cleanup warning"
            )
        }
    };
    let integration = query::load_latest_caller_integration_event(transaction, loop_id)?
        .ok_or_else(|| {
            anyhow!("cannot materialize success result before caller integration is recorded")
        })?;
    let active_checkpoints = query::load_active_checkpoint_state(transaction, loop_id)?;
    let accepted_commits = active_checkpoints
        .iter()
        .map(|checkpoint| {
            Ok((
                checkpoint.sequence_index,
                checkpoint.checkpoint_id.clone(),
                checkpoint
                    .accepted_commit_sha
                    .clone()
                    .ok_or_else(|| anyhow!("accepted checkpoint missing accepted commit sha"))?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let result_generated_at = system::timestamp()?;
    let artifact_summary = accepted_commits
        .iter()
        .map(|(_, checkpoint_id, commit_sha)| {
            let artifact_material = query::load_accepted_artifact_material(
                transaction,
                loop_id,
                checkpoint_id,
                commit_sha,
            )?;
            Ok(json!({
                "checkpoint_id": checkpoint_id,
                "checkpoint_title": artifact_material.title,
                "accepted_commit_sha": commit_sha,
                "change_summary": artifact_material.change_summary,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let commit_summary = accepted_commits
        .iter()
        .map(|(_, checkpoint_id, commit_sha)| {
            let artifact_material =
                query::load_accepted_artifact_material(transaction, loop_id, checkpoint_id, commit_sha)?;
            Ok(json!({
                "checkpoint_id": checkpoint_id,
                "commit_sha": commit_sha,
                "explanation": format!("Accepted artifact for checkpoint '{}'", artifact_material.title),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let improvement_opportunities =
        query::load_caller_facing_improvement_summaries(transaction, loop_id)?;
    let mut result_payload = json!({
        "loop_id": loop_id,
        "status": "success",
        "artifact_summary": artifact_summary,
        "commit_summary": commit_summary,
        "improvement_opportunities": improvement_opportunities,
        "integration_summary": {
            "caller_branch": integration.caller_branch,
            "final_head_sha": integration.final_head_sha,
            "strategy": integration.strategy,
            "landed_commit_shas": integration.landed_commit_shas,
            "resolution_notes": integration.resolution_notes,
        },
        "result_generated_at": result_generated_at,
    });
    if let Some(cleanup_warning) = cleanup_warning {
        result_payload["cleanup_warnings"] = json!([{
            "summary": cleanup_warning.summary,
            "cleanup_disposition": "manual_cleanup_required",
            "worktree_ref": {
                "path": cleanup_warning.worktree_path,
                "branch": cleanup_warning.worktree_branch,
                "label": cleanup_warning.worktree_label,
            },
        }]);
    }
    let result_ref =
        projection::store_json_content(transaction, "result_payload", &result_payload)?;
    projection::append_event(
        transaction,
        loop_id,
        "SUBMIT_LOOP__loop_succeeded",
        &json!({
            "phase": "completed",
        }),
    )?;
    projection::append_event(
        transaction,
        loop_id,
        "CORE__result_materialized",
        &json!({
            "status": "success",
            "result_ref": result_ref,
            "generated_at": result_generated_at,
        }),
    )?;
    Ok(result_payload)
}

fn ensure_success_result_ready_for_caller_finalize(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<()> {
    let loop_state = query::load_loop_state(transaction, loop_id)?;
    if loop_state.status == "failed" {
        bail!("cannot build success result for a failed loop");
    }
    if loop_state.phase != "caller_finalizing" {
        bail!(
            "cannot finalize success from loop phase {}",
            loop_state.phase
        );
    }

    let active_checkpoints = query::load_active_checkpoint_state(transaction, loop_id)?;
    if active_checkpoints.is_empty() {
        bail!("cannot build success result before accepted artifact state exists");
    }
    if active_checkpoints.iter().any(|checkpoint| {
        checkpoint.execution_state != "accepted" || checkpoint.accepted_commit_sha.is_none()
    }) {
        bail!(
            "cannot build success result before every active checkpoint in the executable plan is accepted"
        );
    }

    Ok(())
}

fn validate_caller_integration_summary(
    transaction: &Transaction<'_>,
    runtime: &Runtime,
    loop_id: &str,
    integration_summary: &CallerIntegrationSummary,
    final_head_sha: &str,
) -> Result<()> {
    let last_landed_commit = integration_summary
        .landed_commit_shas
        .last()
        .ok_or_else(|| {
            anyhow!("caller integration summary must include at least one landed commit")
        })?;
    if last_landed_commit != final_head_sha {
        bail!(
            "caller integration summary must end at current HEAD {}; last landed commit was {}",
            final_head_sha,
            last_landed_commit
        );
    }
    for landed_commit_sha in &integration_summary.landed_commit_shas {
        system::git_verify(
            runtime.workspace_root(),
            &[
                "merge-base",
                "--is-ancestor",
                landed_commit_sha,
                final_head_sha,
            ],
        )
        .with_context(|| {
            format!(
                "landed commit {} is not reachable from current HEAD {}",
                landed_commit_sha, final_head_sha
            )
        })?;
    }
    let required_paths = query::load_active_checkpoint_state(transaction, loop_id)?
        .into_iter()
        .map(|checkpoint| {
            let accepted_commit_sha = checkpoint
                .accepted_commit_sha
                .clone()
                .ok_or_else(|| anyhow!("accepted checkpoint missing accepted commit sha"))?;
            let artifact = query::load_accepted_artifact_material(
                transaction,
                loop_id,
                &checkpoint.checkpoint_id,
                &accepted_commit_sha,
            )?;
            let artifact_files = artifact
                .change_summary
                .get("files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if artifact_files.is_empty() {
                Ok(checkpoint
                    .deliverables
                    .into_iter()
                    .filter(|deliverable| deliverable.deliverable_type == "file")
                    .map(|deliverable| deliverable.path)
                    .collect::<Vec<_>>())
            } else {
                Ok(artifact_files)
            }
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if !required_paths.is_empty() {
        let mut changed_paths = HashSet::new();
        for landed_commit_sha in &integration_summary.landed_commit_shas {
            changed_paths.extend(git_changed_paths(
                runtime.workspace_root(),
                landed_commit_sha,
            )?);
        }
        let missing_paths = required_paths
            .into_iter()
            .filter(|path| !changed_paths.contains(path))
            .collect::<Vec<_>>();
        if !missing_paths.is_empty() {
            bail!(
                "caller integration summary does not cover accepted deliverable paths {:?}",
                missing_paths
            );
        }
    }
    Ok(())
}

fn git_changed_paths(workspace_root: &Path, commit_sha: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args([
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-m",
            "--first-parent",
            commit_sha,
        ])
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("failed to inspect changed paths for commit {commit_sha}"))?;
    if !output.status.success() {
        bail!(
            "git diff-tree --no-commit-id --name-only -r -m --first-parent {commit_sha} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}
