// Loop ops own loop lifecycle and result-building workflows; query helpers stay in sibling modules.

use super::super::{projection, query, roles, system, *};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) fn open_loop(runtime: &Runtime, request: OpenLoopRequest) -> Result<OpenLoopResponse> {
    let skill_root = runtime.installed_skill_root()?;
    let manifest = roles::load_manifest(&skill_root)?;
    let coordinator_prompt = request.coordinator_prompt.clone();
    let normalized_input = roles::normalize_open_loop_input(&skill_root, &manifest, request)?;

    let db_path = runtime.db_path()?;
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;

    let loop_id = format!("loop-{}", Uuid::now_v7());
    let branch = format!("loopy-{loop_id}");
    let label = format!("submit-{}", &loop_id[5..]);
    let reserved_worktree_path = runtime
        .workspace_root
        .join(".loopy")
        .join("worktrees")
        .join(&label);

    let loop_input_ref = projection::store_json_content(
        &transaction,
        "loop_input",
        &json!({
            "summary": normalized_input.summary,
            "task_type": normalized_input.task_type,
            "context": normalized_input.context,
            "constraints": normalized_input.constraints,
            "bypass_sandbox": normalized_input.bypass_sandbox,
        }),
    )?;
    let resolved_role_selection_ref = projection::store_json_content(
        &transaction,
        "resolved_role_selection",
        &json!({
            "task_type": normalized_input.resolved_role_selection.task_type,
            "planning_worker": normalized_input.resolved_role_selection.planning_worker,
            "artifact_worker": normalized_input.resolved_role_selection.artifact_worker,
            "checkpoint_reviewers": normalized_input.resolved_role_selection.checkpoint_reviewers,
            "artifact_reviewers": normalized_input.resolved_role_selection.artifact_reviewers,
        }),
    )?;
    let coordinator_role_ref = projection::store_json_content(
        &transaction,
        "role_definition",
        &json!({
            "role": "coordinator",
            "prompt": coordinator_prompt,
        }),
    )?;

    let payload = json!({
        "loop_input_ref": loop_input_ref,
        "resolved_role_selection_ref": resolved_role_selection_ref,
        "coordinator_role_ref": coordinator_role_ref,
        "worktree_path": reserved_worktree_path,
        "worktree_branch": branch,
        "worktree_label": label,
        "base_commit_sha": system::git_head_sha(&runtime.workspace_root)?,
        "phase": "awaiting_worktree",
        "status": "open",
    });
    projection::append_event(&transaction, &loop_id, "SUBMIT_LOOP__loop_opened", &payload)?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(OpenLoopResponse {
        loop_id,
        branch,
        label,
        db_path,
    })
}

pub(crate) fn prepare_worktree(
    runtime: &Runtime,
    request: PrepareWorktreeRequest,
) -> Result<PrepareWorktreeResponse> {
    if let Some(existing_result) = load_existing_result_payload(runtime, &request.loop_id)? {
        if existing_result.get("status").and_then(Value::as_str) == Some("failure") {
            return Ok(existing_result);
        }
    }

    let (loop_state, worktree_lifecycle) = {
        let mut connection = runtime.open_connection()?;
        let transaction = begin_immediate_transaction(&mut connection)?;
        let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
        query::ensure_loop_status_is_open(&loop_state, "prepare worktree")?;
        if query::load_caller_finalize_status(&transaction, &request.loop_id)?.is_some() {
            bail!("cannot prepare worktree after caller finalize handoff");
        }
        let worktree_lifecycle = query::load_worktree_lifecycle(&transaction, &request.loop_id)?;
        transaction.commit()?;
        (loop_state, worktree_lifecycle)
    };

    let worktree_path = PathBuf::from(&loop_state.worktree_path);
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let worktree_path_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 worktree path {}", worktree_path.display()))?;
    if matches!(
        worktree_lifecycle.as_deref(),
        Some("prepared") | Some("created")
    ) && existing_worktree_matches_branch(runtime, &loop_state, worktree_path_str)?
    {
        return Ok(prepared_worktree_payload(&request.loop_id, &loop_state));
    }
    if existing_worktree_matches_branch(runtime, &loop_state, worktree_path_str)? {
        return record_prepared_worktree(runtime, &request.loop_id, &loop_state);
    }
    if !worktree_path.try_exists()?
        && registered_disposable_worktree(runtime, worktree_path_str, &loop_state.worktree_label)?
    {
        remove_disposable_worktree_registration(
            runtime,
            worktree_path_str,
            &loop_state.worktree_label,
        )?;
    }
    let prepare_result = prepare_disposable_worktree(runtime, &loop_state, worktree_path_str);
    match prepare_result {
        Ok(()) => record_prepared_worktree(runtime, &request.loop_id, &loop_state),
        Err(error) => {
            if existing_worktree_matches_branch(runtime, &loop_state, worktree_path_str)? {
                return record_prepared_worktree(runtime, &request.loop_id, &loop_state);
            }
            if retryable_for_mirrored_gitdir_fallback(&error) {
                return Err(error);
            }
            let summary = format!("{error:#}");
            let mut connection = runtime.open_connection()?;
            let transaction = begin_immediate_transaction(&mut connection)?;
            projection::append_event(
                &transaction,
                &request.loop_id,
                "SUBMIT_LOOP__worktree_prepare_failed",
                &json!({
                    "worktree_path": loop_state.worktree_path,
                    "worktree_branch": loop_state.worktree_branch,
                    "worktree_label": loop_state.worktree_label,
                    "error_message": summary,
                    "phase": loop_state.phase,
                }),
            )?;
            let result = append_failure_result(
                &transaction,
                &request.loop_id,
                &loop_state,
                "worktree_prepare_failed",
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
    }
}

fn record_prepared_worktree(
    runtime: &Runtime,
    loop_id: &str,
    loop_state: &LoopState,
) -> Result<PrepareWorktreeResponse> {
    let phase_after_prepare = if loop_state.phase == "awaiting_worktree" {
        "planning"
    } else {
        loop_state.phase.as_str()
    };
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    projection::append_event(
        &transaction,
        loop_id,
        "SUBMIT_LOOP__worktree_prepared",
        &json!({
            "worktree_path": loop_state.worktree_path,
            "worktree_branch": loop_state.worktree_branch,
            "worktree_label": loop_state.worktree_label,
            "phase": phase_after_prepare,
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;
    Ok(prepared_worktree_payload(loop_id, loop_state))
}

fn existing_worktree_matches_branch(
    runtime: &Runtime,
    loop_state: &LoopState,
    worktree_path_str: &str,
) -> Result<bool> {
    let worktree_path = Path::new(worktree_path_str);
    if !worktree_path.try_exists()? {
        return Ok(false);
    }
    if registered_disposable_worktree(runtime, worktree_path_str, &loop_state.worktree_label)? {
        return worktree_head_matches_branch(runtime, worktree_path, worktree_path_str, loop_state);
    }
    mirrored_worktree_metadata_matches_branch(runtime, worktree_path_str, loop_state)
}

fn worktree_head_matches_branch(
    runtime: &Runtime,
    worktree_path: &Path,
    worktree_path_str: &str,
    loop_state: &LoopState,
) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", worktree_path_str, "rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&runtime.workspace_root)
        .output()
        .with_context(|| {
            format!(
                "failed to inspect existing worktree {}",
                worktree_path.display()
            )
        })?;
    if !output.status.success() {
        return Ok(false);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == loop_state.worktree_branch)
}

fn mirrored_worktree_metadata_matches_branch(
    runtime: &Runtime,
    worktree_path_str: &str,
    loop_state: &LoopState,
) -> Result<bool> {
    let worktree_path = Path::new(worktree_path_str);
    if mirrored_gitdir_path_from_worktree_metadata(runtime, worktree_path_str, &loop_state.worktree_label)?
        .is_none()
    {
        return Ok(false);
    }
    worktree_head_matches_branch(runtime, worktree_path, worktree_path_str, loop_state)
}

fn worktree_branch_exists(runtime: &Runtime, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(&runtime.workspace_root)
        .output()
        .with_context(|| format!("failed to inspect worktree branch {branch}"))?;
    Ok(output.status.success())
}

fn prepare_disposable_worktree(
    runtime: &Runtime,
    loop_state: &LoopState,
    worktree_path_str: &str,
) -> Result<()> {
    let primary_result = if worktree_branch_exists(runtime, &loop_state.worktree_branch)? {
        system::git_verify(
            &runtime.workspace_root,
            &[
                "worktree",
                "add",
                worktree_path_str,
                &loop_state.worktree_branch,
            ],
        )
    } else {
        system::git_verify(
            &runtime.workspace_root,
            &[
                "worktree",
                "add",
                "-b",
                &loop_state.worktree_branch,
                worktree_path_str,
                &loop_state.base_commit_sha,
            ],
        )
    };
    match primary_result {
        Ok(()) => Ok(()),
        Err(error) => {
            if !retryable_for_mirrored_gitdir_fallback(&error) {
                return Err(error);
            }
            prepare_disposable_worktree_with_mirrored_gitdir_fallback(
                runtime,
                loop_state,
                worktree_path_str,
            )
            .with_context(|| {
                format!("primary gitdir prepare-worktree failed before mirrored fallback: {error:#}")
            })
        }
    }
}

fn prepare_disposable_worktree_with_mirrored_gitdir_fallback(
    runtime: &Runtime,
    loop_state: &LoopState,
    worktree_path_str: &str,
) -> Result<()> {
    if mirrored_worktree_metadata_matches_branch(runtime, worktree_path_str, loop_state)? {
        return Ok(());
    }

    let mirror_path = mirrored_gitdir_path(runtime, &loop_state.worktree_label);
    if !mirror_path.try_exists()? {
        fs::create_dir_all(&mirror_path)
            .with_context(|| format!("failed to create {}", mirror_path.display()))?;
        let source_git_dir = workspace_gitdir_source_path(runtime)?;
        copy_directory_contents(&source_git_dir, &mirror_path)?;
    }
    ensure_mirrored_gitdir_write_paths(&mirror_path)?;

    let git_dir_arg = format!("--git-dir={}", mirror_path.display());
    let work_tree_arg = format!("--work-tree={}", runtime.workspace_root.display());
    let mut args = vec![
        git_dir_arg,
        work_tree_arg,
        "worktree".to_owned(),
        "add".to_owned(),
    ];
    if worktree_branch_exists(runtime, &loop_state.worktree_branch)? {
        args.push(worktree_path_str.to_owned());
        args.push(loop_state.worktree_branch.clone());
    } else {
        args.push("-b".to_owned());
        args.push(loop_state.worktree_branch.clone());
        args.push(worktree_path_str.to_owned());
        args.push(loop_state.base_commit_sha.clone());
    }
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    match system::git_verify(&runtime.workspace_root, &arg_refs) {
        Ok(()) => Ok(()),
        Err(error) => {
            if mirrored_worktree_metadata_matches_branch(runtime, worktree_path_str, loop_state)? {
                return Ok(());
            }
            Err(error)
        }
    }
}

fn workspace_gitdir_source_path(runtime: &Runtime) -> Result<PathBuf> {
    let git_path = runtime.workspace_root.join(".git");
    if git_path.is_dir() {
        return Ok(git_path);
    }
    let metadata = fs::read_to_string(&git_path)
        .with_context(|| format!("failed to read {}", git_path.display()))?;
    let Some(gitdir_value) = metadata.strip_prefix("gitdir:") else {
        bail!("workspace git metadata {} did not contain a gitdir pointer", git_path.display());
    };
    let gitdir_path = PathBuf::from(gitdir_value.trim());
    if gitdir_path.is_absolute() {
        Ok(gitdir_path)
    } else {
        Ok(runtime.workspace_root.join(gitdir_path))
    }
}

fn copy_directory_contents(source_root: &Path, destination_root: &Path) -> Result<()> {
    fs::create_dir_all(destination_root)
        .with_context(|| format!("failed to create {}", destination_root.display()))?;
    for entry in fs::read_dir(source_root)
        .with_context(|| format!("failed to read {}", source_root.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination_root.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_directory_contents(&source_path, &destination_path)?;
            let permissions = fs::metadata(&source_path)?.permissions();
            fs::set_permissions(&destination_path, permissions).with_context(|| {
                format!("failed to set permissions on {}", destination_path.display())
            })?;
        } else {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
            let permissions = fs::metadata(&source_path)?.permissions();
            fs::set_permissions(&destination_path, permissions).with_context(|| {
                format!("failed to set permissions on {}", destination_path.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_mirrored_gitdir_write_paths(mirror_path: &Path) -> Result<()> {
    for directory in [
        mirror_path.to_path_buf(),
        mirror_path.join("refs"),
        mirror_path.join("refs").join("heads"),
        mirror_path.join("logs"),
        mirror_path.join("logs").join("refs"),
        mirror_path.join("logs").join("refs").join("heads"),
        mirror_path.join("worktrees"),
    ] {
        if !directory.try_exists()? {
            fs::create_dir_all(&directory)
                .with_context(|| format!("failed to create {}", directory.display()))?;
        }
        let metadata = fs::metadata(&directory)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(&directory, permissions)
            .with_context(|| format!("failed to relax {}", directory.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_mirrored_gitdir_write_paths(_mirror_path: &Path) -> Result<()> {
    Ok(())
}

fn retryable_for_mirrored_gitdir_fallback(error: &anyhow::Error) -> bool {
    let summary = format!("{error:#}").to_ascii_lowercase();
    let blocked_gitdir_write = summary.contains(".git/worktrees/")
        || summary.contains(".git/worktrees\\")
        || summary.contains(".git/refs/heads/")
        || summary.contains(".git/refs/heads\\")
        || summary.contains(".git/logs/refs/heads/")
        || summary.contains(".git/logs/refs/heads\\")
        || summary.contains("cannot lock ref 'refs/heads/")
        || summary.contains("cannot update the ref 'refs/heads/");
    let permission_or_fs_block = summary.contains("permission denied")
        || summary.contains("operation not permitted")
        || summary.contains("read-only file system");
    blocked_gitdir_write && permission_or_fs_block
}

pub(crate) fn open_review_round(
    runtime: &Runtime,
    request: OpenReviewRoundRequest,
) -> Result<OpenReviewRoundResponse> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    if query::load_caller_finalize_status(&transaction, &request.loop_id)?.is_some() {
        bail!("cannot open review rounds after caller finalize handoff");
    }
    let skill_root = runtime.installed_skill_root()?;
    let resolved_role_selection = query::load_resolved_role_selection(
        &transaction,
        &skill_root,
        &loop_state.resolved_role_selection_ref,
    )?;
    let reviewer_role_ids = match request.review_kind {
        ReviewKind::Checkpoint => resolved_role_selection.checkpoint_reviewers.clone(),
        ReviewKind::Artifact => resolved_role_selection.artifact_reviewers.clone(),
    };
    if reviewer_role_ids.is_empty() {
        bail!("review rounds require at least one reviewer role");
    }
    let duplicate_review_round_id: Option<String> = transaction
        .query_row(
            r#"
            SELECT review_round_id
            FROM SUBMIT_LOOP__review_current
            WHERE loop_id = ?1
              AND review_kind = ?2
              AND round_status = 'pending'
              AND target_type = ?3
              AND target_ref = ?4
            LIMIT 1
            "#,
            params![
                request.loop_id,
                request.review_kind.as_str(),
                request.target_type,
                request.target_ref
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(review_round_id) = duplicate_review_round_id {
        bail!(
            "pending review round {} already exists for {} {} {}",
            review_round_id,
            request.review_kind.as_str(),
            request.target_type,
            request.target_ref
        );
    }
    let target_metadata = match request.review_kind {
        ReviewKind::Checkpoint => query::snapshot_checkpoint_review_target(
            &transaction,
            &request.loop_id,
            &request.target_type,
            &request.target_ref,
        )?,
        ReviewKind::Artifact => query::snapshot_artifact_review_target(
            &transaction,
            &request.loop_id,
            &request.target_type,
            &request.target_ref,
        )?,
    };

    let review_round_id = format!("review-{}", Uuid::now_v7());
    let review_slot_ids = reviewer_role_ids
        .iter()
        .map(|_| format!("slot-{}", Uuid::now_v7()))
        .collect::<Vec<_>>();
    let slot_state = review_slot_ids
        .iter()
        .zip(reviewer_role_ids.iter())
        .map(|(review_slot_id, reviewer_role_id)| {
            json!({
                "review_slot_id": review_slot_id,
                "reviewer_role_id": reviewer_role_id,
                "status": "pending",
            })
        })
        .collect::<Vec<_>>();

    projection::append_event(
        &transaction,
        &request.loop_id,
        "SUBMIT_LOOP__review_round_opened",
        &json!({
            "review_round_id": review_round_id,
            "review_kind": request.review_kind.as_str(),
            "round_status": "pending",
            "target_type": request.target_type,
            "target_ref": request.target_ref,
            "target_metadata": target_metadata,
            "slot_state": slot_state,
        }),
    )?;
    projection::rebuild_all_projections(&transaction)?;
    transaction.commit()?;

    Ok(OpenReviewRoundResponse {
        review_round_id,
        review_slot_ids,
    })
}

pub(crate) fn finalize_failure(
    runtime: &Runtime,
    request: FinalizeFailureRequest,
) -> Result<FinalizeFailureResponse> {
    if let Some(existing_result) = load_existing_result_payload(runtime, &request.loop_id)? {
        match existing_result.get("status").and_then(Value::as_str) {
            Some("failure") => return Ok(existing_result),
            Some("success") => {
                bail!(
                    "cannot finalize failure after a success result has already been materialized"
                )
            }
            Some(status) => bail!("cannot finalize failure with existing result status {status}"),
            None => {
                bail!(
                    "cannot finalize failure because the existing result payload is missing a status"
                )
            }
        }
    }

    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let loop_state = query::load_loop_state(&transaction, &request.loop_id)?;
    if loop_state.status == "failed" {
        let failure_event = query::load_latest_failure_event(&transaction, &request.loop_id)?
            .ok_or_else(|| {
                anyhow!("cannot materialize failure result before a failure event exists")
            })?;
        let result = materialize_failure_result(
            &transaction,
            &request.loop_id,
            &loop_state,
            failure_event.event_id,
            &failure_event.failure_cause_type,
            &failure_event.summary,
            &failure_event.phase_at_failure,
            &failure_event.last_stable_context,
        )?;
        projection::rebuild_all_projections(&transaction)?;
        transaction.commit()?;
        return Ok(result);
    }
    let result = append_failure_result(
        &transaction,
        &request.loop_id,
        &loop_state,
        &request.failure_cause_type,
        &request.summary,
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

pub(crate) fn load_existing_result_payload(
    runtime: &Runtime,
    loop_id: &str,
) -> Result<Option<Value>> {
    let mut connection = runtime.open_connection()?;
    let transaction = begin_immediate_transaction(&mut connection)?;
    let result = query::load_existing_result_ref(&transaction, loop_id)?
        .map(|result_ref| query::load_json_content(&transaction, &result_ref))
        .transpose()?;
    transaction.commit()?;
    Ok(result)
}

fn prepared_worktree_payload(loop_id: &str, loop_state: &LoopState) -> Value {
    json!({
        "loop_id": loop_id,
        "path": loop_state.worktree_path,
        "branch": loop_state.worktree_branch,
        "label": loop_state.worktree_label,
        "lifecycle": "prepared",
    })
}

pub(crate) enum WorktreeCleanupOutcome {
    Deleted,
    Warning { summary: String },
}

pub(crate) fn append_failure_result(
    transaction: &Transaction<'_>,
    loop_id: &str,
    loop_state: &LoopState,
    failure_cause_type: &str,
    summary: &str,
    phase_at_failure: &str,
    last_stable_context: &Value,
) -> Result<Value> {
    projection::append_event(
        transaction,
        loop_id,
        "SUBMIT_LOOP__loop_failed",
        &json!({
            "failure_cause_type": failure_cause_type,
            "summary": summary,
            "phase_at_failure": phase_at_failure,
            "last_stable_context": last_stable_context,
        }),
    )?;
    let source_event_id = latest_loop_event_id(transaction, loop_id)?;
    materialize_failure_result(
        transaction,
        loop_id,
        loop_state,
        source_event_id,
        failure_cause_type,
        summary,
        phase_at_failure,
        last_stable_context,
    )
}

fn materialize_failure_result(
    transaction: &Transaction<'_>,
    loop_id: &str,
    loop_state: &LoopState,
    source_event_id: i64,
    failure_cause_type: &str,
    summary: &str,
    phase_at_failure: &str,
    last_stable_context: &Value,
) -> Result<Value> {
    if let Some(existing_result_ref) = query::load_existing_result_ref(transaction, loop_id)? {
        return query::load_json_content(transaction, &existing_result_ref);
    }

    let result_generated_at = system::timestamp()?;
    let result_payload = json!({
        "loop_id": loop_id,
        "status": "failure",
        "failure_cause_type": failure_cause_type,
        "summary": summary,
        "source_event_id": source_event_id,
        "phase_at_failure": phase_at_failure,
        "last_stable_context": last_stable_context,
        "worktree_ref": {
            "path": loop_state.worktree_path,
            "branch": loop_state.worktree_branch,
            "label": loop_state.worktree_label,
        },
        "result_generated_at": result_generated_at,
    });
    let result_ref =
        projection::store_json_content(transaction, "result_payload", &result_payload)?;
    projection::append_event(
        transaction,
        loop_id,
        "CORE__result_materialized",
        &json!({
            "status": "failure",
            "result_ref": result_ref,
            "generated_at": result_generated_at,
        }),
    )?;
    Ok(result_payload)
}

pub(crate) fn append_worktree_cleanup_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
    loop_state: &LoopState,
    cleanup_outcome: &WorktreeCleanupOutcome,
) -> Result<()> {
    match cleanup_outcome {
        WorktreeCleanupOutcome::Deleted => {
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__worktree_deleted",
                &json!({
                    "worktree_path": loop_state.worktree_path,
                    "worktree_branch": loop_state.worktree_branch,
                    "worktree_label": loop_state.worktree_label,
                }),
            )?;
        }
        WorktreeCleanupOutcome::Warning { summary } => {
            projection::append_event(
                transaction,
                loop_id,
                "SUBMIT_LOOP__worktree_cleanup_warning",
                &json!({
                    "summary": summary,
                    "worktree_path": loop_state.worktree_path,
                    "worktree_branch": loop_state.worktree_branch,
                    "worktree_label": loop_state.worktree_label,
                }),
            )?;
        }
    }
    Ok(())
}

fn latest_loop_event_id(transaction: &Transaction<'_>, loop_id: &str) -> Result<i64> {
    transaction
        .query_row(
            "SELECT MAX(event_id) FROM CORE__events WHERE loop_id = ?1",
            [loop_id],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .ok_or_else(|| anyhow!("loop {loop_id} has no events"))
}

pub(crate) fn cleanup_disposable_worktree(
    runtime: &Runtime,
    worktree_path: &Path,
    worktree_label: &str,
) -> Result<()> {
    let worktree_path_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 worktree path {}", worktree_path.display()))?;
    if !worktree_path.try_exists()? {
        if registered_disposable_worktree(runtime, worktree_path_str, worktree_label)? {
            return remove_disposable_worktree_registration(
                runtime,
                worktree_path_str,
                worktree_label,
            );
        }
        return Ok(());
    }
    if !registered_disposable_worktree(runtime, worktree_path_str, worktree_label)? {
        remove_orphaned_disposable_worktree_directory(worktree_path)?;
        return Ok(());
    }

    let first_cleanup = attempt_disposable_worktree_cleanup(runtime, worktree_path, worktree_label);
    if first_cleanup.is_ok() || !worktree_path.try_exists()? {
        return Ok(());
    }
    if recover_prunable_disposable_worktree(runtime, worktree_path, worktree_label)? {
        return Ok(());
    }

    best_effort_abort_unfinished_git_operations(runtime, worktree_path)?;
    let second_cleanup =
        attempt_disposable_worktree_cleanup(runtime, worktree_path, worktree_label);
    if second_cleanup.is_ok() || !worktree_path.try_exists()? {
        return Ok(());
    }
    if recover_prunable_disposable_worktree(runtime, worktree_path, worktree_label)? {
        return Ok(());
    }
    second_cleanup
}

fn attempt_disposable_worktree_cleanup(
    runtime: &Runtime,
    worktree_path: &Path,
    worktree_label: &str,
) -> Result<()> {
    let worktree_path_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 worktree path {}", worktree_path.display()))?;
    system::git_verify(
        &runtime.workspace_root,
        &["-C", worktree_path_str, "reset", "--hard"],
    )?;
    // Linked worktrees store their gitdir pointer as a top-level `.git` file.
    // Preserve that file so `git worktree remove --force` can still validate
    // and deregister the worktree after cleaning untracked contents.
    system::git_verify(
        &runtime.workspace_root,
        &["-C", worktree_path_str, "clean", "-ffdx", "-e", "/.git"],
    )?;
    remove_disposable_worktree_registration(runtime, worktree_path_str, worktree_label)
}

fn recover_prunable_disposable_worktree(
    runtime: &Runtime,
    worktree_path: &Path,
    worktree_label: &str,
) -> Result<bool> {
    let git_metadata_path = worktree_path.join(".git");
    if git_metadata_path.try_exists()? {
        return Ok(false);
    }

    let worktree_path_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 worktree path {}", worktree_path.display()))?;
    let worktree_line = format!("worktree {worktree_path_str}");

    match locate_disposable_worktree_registration(runtime, worktree_path_str, worktree_label)? {
        Some(DisposableWorktreeRegistration::Mirror(mirrored_gitdir)) => {
            // `git worktree remove --force` can leave a mirrored registration in a
            // prunable state after it has already removed the linked worktree's `.git`
            // pointer. Prune that stale registration, then delete the dedicated
            // mirror and any leftover worktree directory.
            prune_disposable_worktree_registration(runtime, Some(&mirrored_gitdir))?;
            if let Some(listing) = disposable_worktree_listing(runtime, Some(&mirrored_gitdir))? {
                if listing.lines().any(|line| line == worktree_line) {
                    return Ok(false);
                }
            }
            if worktree_path.try_exists()? {
                remove_orphaned_disposable_worktree_directory(worktree_path)?;
            }
            if mirrored_gitdir.try_exists()? {
                fs::remove_dir_all(&mirrored_gitdir).with_context(|| {
                    format!(
                        "failed to remove mirrored gitdir {} after pruning disposable worktree registration",
                        mirrored_gitdir.display()
                    )
                })?;
            }
            Ok(true)
        }
        Some(DisposableWorktreeRegistration::Primary) => {
            prune_disposable_worktree_registration(runtime, None)?;
            let listing = disposable_worktree_listing(runtime, None)?
                .ok_or_else(|| anyhow!("primary git worktree listing unexpectedly unavailable"))?;
            if listing.lines().any(|line| line == worktree_line) {
                return Ok(false);
            }
            if worktree_path.try_exists()? {
                remove_orphaned_disposable_worktree_directory(worktree_path)?;
            }
            Ok(true)
        }
        None => {
            if worktree_path.try_exists()? {
                remove_orphaned_disposable_worktree_directory(worktree_path)?;
            }
            Ok(true)
        }
    }
}

fn prune_disposable_worktree_registration(
    runtime: &Runtime,
    mirrored_gitdir: Option<&Path>,
) -> Result<()> {
    let output = if let Some(mirrored_gitdir) = mirrored_gitdir {
        if !mirrored_gitdir.try_exists()? {
            return Ok(());
        }
        let git_dir_arg = format!("--git-dir={}", mirrored_gitdir.display());
        let work_tree_arg = format!("--work-tree={}", runtime.workspace_root.display());
        let output = Command::new("git")
            .args([
                git_dir_arg.as_str(),
                work_tree_arg.as_str(),
                "worktree",
                "prune",
                "--verbose",
            ])
            .current_dir(&runtime.workspace_root)
            .output()
            .with_context(|| {
                format!(
                    "failed to run git {} {} worktree prune --verbose",
                    git_dir_arg, work_tree_arg
                )
            })?;
        if !output.status.success() {
            bail!(
                "git {} {} worktree prune --verbose failed\nstdout:\n{}\nstderr:\n{}",
                git_dir_arg,
                work_tree_arg,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output
    } else {
        let output = Command::new("git")
            .args(["worktree", "prune", "--verbose"])
            .current_dir(&runtime.workspace_root)
            .output()
            .context("failed to run git worktree prune --verbose")?;
        if !output.status.success() {
            bail!(
                "git worktree prune --verbose failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output
    };
    let _ = output;
    Ok(())
}

fn remove_disposable_worktree_registration(
    runtime: &Runtime,
    worktree_path_str: &str,
    worktree_label: &str,
) -> Result<()> {
    match locate_disposable_worktree_registration(runtime, worktree_path_str, worktree_label)? {
        Some(DisposableWorktreeRegistration::Mirror(mirrored_gitdir)) => {
            let git_dir_arg = format!("--git-dir={}", mirrored_gitdir.display());
            let work_tree_arg = format!("--work-tree={}", runtime.workspace_root.display());
            let output = Command::new("git")
                .args([
                    git_dir_arg.as_str(),
                    work_tree_arg.as_str(),
                    "worktree",
                    "remove",
                    "--force",
                    worktree_path_str,
                ])
                .current_dir(&runtime.workspace_root)
                .output()
                .with_context(|| {
                    format!(
                        "failed to run git {} {} worktree remove --force {}",
                        git_dir_arg, work_tree_arg, worktree_path_str
                    )
                })?;
            if output.status.success() {
                if mirrored_gitdir.try_exists()? {
                    fs::remove_dir_all(&mirrored_gitdir).with_context(|| {
                        format!(
                            "failed to remove mirrored gitdir {}",
                            mirrored_gitdir.display()
                        )
                    })?;
                }
                return Ok(());
            }
            bail!(
                "git {} {} worktree remove --force {} failed\nstdout:\n{}\nstderr:\n{}",
                git_dir_arg,
                work_tree_arg,
                worktree_path_str,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Some(DisposableWorktreeRegistration::Primary) => system::git_verify(
            &runtime.workspace_root,
            &["worktree", "remove", "--force", worktree_path_str],
        ),
        None => {
            let worktree_path = Path::new(worktree_path_str);
            if worktree_path.try_exists()? {
                remove_orphaned_disposable_worktree_directory(worktree_path)?;
            }
            Ok(())
        }
    }
}

fn remove_orphaned_disposable_worktree_directory(worktree_path: &Path) -> Result<()> {
    fs::remove_dir_all(worktree_path).with_context(|| {
        format!(
            "failed to remove orphaned disposable worktree directory {}",
            worktree_path.display()
        )
    })
}

fn registered_disposable_worktree(
    runtime: &Runtime,
    worktree_path_str: &str,
    worktree_label: &str,
) -> Result<bool> {
    Ok(
        locate_disposable_worktree_registration(runtime, worktree_path_str, worktree_label)?
            .is_some(),
    )
}

enum DisposableWorktreeRegistration {
    Mirror(PathBuf),
    Primary,
}

pub(crate) fn authoritative_worktree_git_dir(
    runtime: &Runtime,
    worktree_path_str: &str,
    worktree_label: &str,
) -> Result<PathBuf> {
    match locate_disposable_worktree_registration(runtime, worktree_path_str, worktree_label)? {
        Some(DisposableWorktreeRegistration::Mirror(mirrored_gitdir)) => Ok(mirrored_gitdir),
        Some(DisposableWorktreeRegistration::Primary) | None => {
            Ok(runtime.workspace_root.join(".git"))
        }
    }
}

fn locate_disposable_worktree_registration(
    runtime: &Runtime,
    worktree_path_str: &str,
    worktree_label: &str,
) -> Result<Option<DisposableWorktreeRegistration>> {
    let worktree_line = format!("worktree {worktree_path_str}");
    let mirrored_gitdir = mirrored_gitdir_path(runtime, worktree_label);
    if let Some(listing) = disposable_worktree_listing(runtime, Some(&mirrored_gitdir))? {
        if listing.lines().any(|line| line == worktree_line) {
            return Ok(Some(DisposableWorktreeRegistration::Mirror(
                mirrored_gitdir,
            )));
        }
    }
    let primary_listing = disposable_worktree_listing(runtime, None)?
        .ok_or_else(|| anyhow!("primary git worktree listing unexpectedly unavailable"))?;
    if primary_listing.lines().any(|line| line == worktree_line) {
        return Ok(Some(DisposableWorktreeRegistration::Primary));
    }
    if let Some(actual_mirrored_gitdir) =
        mirrored_gitdir_path_from_worktree_metadata(runtime, worktree_path_str, worktree_label)?
    {
        if let Some(listing) = disposable_worktree_listing(runtime, Some(&actual_mirrored_gitdir))?
        {
            if listing.lines().any(|line| line == worktree_line) {
                return Ok(Some(DisposableWorktreeRegistration::Mirror(
                    actual_mirrored_gitdir,
                )));
            }
        }
    }
    Ok(None)
}

fn mirrored_gitdir_path_from_worktree_metadata(
    runtime: &Runtime,
    worktree_path_str: &str,
    worktree_label: &str,
) -> Result<Option<PathBuf>> {
    let git_metadata_path = Path::new(worktree_path_str).join(".git");
    if !git_metadata_path.try_exists()? {
        return Ok(None);
    }
    if git_metadata_path.is_dir() {
        return Ok(None);
    }
    let metadata = fs::read_to_string(&git_metadata_path)
        .with_context(|| format!("failed to read {}", git_metadata_path.display()))?;
    let Some(gitdir_value) = metadata.strip_prefix("gitdir:") else {
        return Ok(None);
    };
    let gitdir_path = PathBuf::from(gitdir_value.trim());
    let loopy_root = runtime.workspace_root.join(".loopy");
    if !gitdir_path.starts_with(&loopy_root) {
        return Ok(None);
    }
    if gitdir_path.file_name().and_then(|name| name.to_str()) != Some(worktree_label) {
        return Ok(None);
    }
    let Some(worktrees_dir) = gitdir_path.parent() else {
        return Ok(None);
    };
    if worktrees_dir.file_name().and_then(|name| name.to_str()) != Some("worktrees") {
        return Ok(None);
    }
    let Some(mirrored_gitdir) = worktrees_dir.parent() else {
        return Ok(None);
    };
    let Some(mirrored_name) = mirrored_gitdir.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !mirrored_name.starts_with("git-common-") {
        return Ok(None);
    }
    Ok(Some(mirrored_gitdir.to_path_buf()))
}

fn disposable_worktree_listing(
    runtime: &Runtime,
    mirrored_gitdir: Option<&Path>,
) -> Result<Option<String>> {
    let output = if let Some(mirrored_gitdir) = mirrored_gitdir {
        if !mirrored_gitdir.try_exists()? {
            return Ok(None);
        }
        let git_dir_arg = format!("--git-dir={}", mirrored_gitdir.display());
        let work_tree_arg = format!("--work-tree={}", runtime.workspace_root.display());
        let output = Command::new("git")
            .args([
                git_dir_arg.as_str(),
                work_tree_arg.as_str(),
                "worktree",
                "list",
                "--porcelain",
            ])
            .current_dir(&runtime.workspace_root)
            .output()
            .with_context(|| {
                format!(
                    "failed to run git {} {} worktree list --porcelain",
                    git_dir_arg, work_tree_arg
                )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            if stderr.contains("not a git repository") {
                return Ok(None);
            }
            bail!(
                "git {} {} worktree list --porcelain failed\nstdout:\n{}\nstderr:\n{}",
                git_dir_arg,
                work_tree_arg,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output
    } else {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&runtime.workspace_root)
            .output()
            .context("failed to run git worktree list --porcelain")?;
        if !output.status.success() {
            bail!(
                "git worktree list --porcelain failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output
    };
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_owned()))
}

fn mirrored_gitdir_path(runtime: &Runtime, worktree_label: &str) -> PathBuf {
    runtime
        .workspace_root
        .join(".loopy")
        .join(format!("git-common-{worktree_label}"))
}

fn best_effort_abort_unfinished_git_operations(
    runtime: &Runtime,
    worktree_path: &Path,
) -> Result<()> {
    let worktree_path_str = worktree_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 worktree path {}", worktree_path.display()))?;
    for args in [
        ["-C", worktree_path_str, "merge", "--abort"],
        ["-C", worktree_path_str, "rebase", "--abort"],
        ["-C", worktree_path_str, "cherry-pick", "--abort"],
        ["-C", worktree_path_str, "am", "--abort"],
    ] {
        let _ = Command::new("git")
            .args(args)
            .current_dir(&runtime.workspace_root)
            .output();
    }
    Ok(())
}
