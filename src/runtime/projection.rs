// Projection logic owns schema bootstrap, event append, and replay into current-state tables only.

use super::*;

const SCHEMA_VERSION: i64 = 2;

pub(crate) fn schema_bootstrap_required(connection: &Connection) -> Result<bool> {
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read SQLite schema version")?;
    Ok(user_version < SCHEMA_VERSION)
}

pub(crate) fn bootstrap_schema(connection: &Connection) -> Result<()> {
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable SQLite WAL mode")?;
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS CORE__events (
            event_id INTEGER PRIMARY KEY AUTOINCREMENT,
            loop_id TEXT NOT NULL,
            loop_seq INTEGER NOT NULL,
            event_name TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            recorded_at TEXT NOT NULL,
            UNIQUE(loop_id, loop_seq)
        );

        CREATE TABLE IF NOT EXISTS CORE__contents (
            content_ref TEXT PRIMARY KEY,
            content_kind TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS CORE__transcript_segments (
            segment_id INTEGER PRIMARY KEY AUTOINCREMENT,
            invocation_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            speaker TEXT NOT NULL,
            segment_kind TEXT NOT NULL,
            summary TEXT,
            content_ref TEXT NOT NULL,
            occurred_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS CORE__invocation_current (
            invocation_id TEXT PRIMARY KEY,
            loop_id TEXT NOT NULL,
            invocation_role TEXT NOT NULL,
            stage TEXT NOT NULL,
            status TEXT NOT NULL,
            token TEXT,
            accepted_api TEXT,
            accepted_submission_id TEXT,
            role_definition_ref TEXT,
            executor_config_ref TEXT,
            invocation_context_ref TEXT,
            review_round_id TEXT,
            review_slot_id TEXT,
            allowed_terminal_apis_json TEXT,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS CORE__capability_current (
            invocation_id TEXT PRIMARY KEY,
            token TEXT NOT NULL UNIQUE,
            token_state TEXT NOT NULL,
            accepted_api TEXT,
            accepted_submission_id TEXT,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS CORE__result_current (
            loop_id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            result_ref TEXT NOT NULL,
            generated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__loop_current (
            loop_id TEXT PRIMARY KEY,
            phase TEXT NOT NULL,
            status TEXT NOT NULL,
            worktree_path TEXT NOT NULL,
            worktree_branch TEXT NOT NULL,
            worktree_label TEXT NOT NULL,
            base_commit_sha TEXT NOT NULL,
            loop_input_ref TEXT NOT NULL,
            resolved_role_selection_ref TEXT,
            coordinator_role_ref TEXT NOT NULL,
            failure_cause_type TEXT,
            failure_summary TEXT,
            latest_event_id INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__plan_current (
            loop_id TEXT PRIMARY KEY,
            latest_submitted_plan_revision INTEGER,
            current_executable_plan_revision INTEGER,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__checkpoint_current (
            loop_id TEXT NOT NULL,
            checkpoint_id TEXT NOT NULL,
            sequence_index INTEGER NOT NULL,
            title TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'artifact',
            deliverables_json TEXT NOT NULL DEFAULT '[]',
            acceptance_json TEXT NOT NULL DEFAULT '{}',
            revision INTEGER NOT NULL,
            execution_state TEXT NOT NULL,
            candidate_commit_sha TEXT,
            accepted_commit_sha TEXT,
            active INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(loop_id, checkpoint_id)
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__review_current (
            loop_id TEXT NOT NULL,
            review_round_id TEXT NOT NULL,
            review_kind TEXT NOT NULL,
            round_status TEXT NOT NULL,
            target_type TEXT NOT NULL,
            target_ref TEXT NOT NULL,
            target_metadata_json TEXT NOT NULL DEFAULT '{}',
            slot_state_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(loop_id, review_round_id)
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__worktree_current (
            loop_id TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            branch TEXT NOT NULL,
            label TEXT NOT NULL,
            lifecycle TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__caller_finalize_current (
            loop_id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            block_context_ref TEXT,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__timeout_extension_current (
            invocation_id TEXT PRIMARY KEY,
            loop_id TEXT NOT NULL,
            latest_request_content_ref TEXT NOT NULL,
            requested_timeout_sec INTEGER NOT NULL,
            progress_summary TEXT NOT NULL,
            rationale TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS SUBMIT_LOOP__commit_current (
            loop_id TEXT NOT NULL,
            checkpoint_id TEXT NOT NULL,
            commit_sha TEXT NOT NULL,
            lifecycle TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(loop_id, checkpoint_id, commit_sha)
        );
    "#,
    )?;
    ensure_text_column(
        connection,
        "SUBMIT_LOOP__checkpoint_current",
        "kind",
        "TEXT NOT NULL DEFAULT 'artifact'",
    )?;
    ensure_text_column(
        connection,
        "SUBMIT_LOOP__checkpoint_current",
        "deliverables_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_text_column(
        connection,
        "SUBMIT_LOOP__checkpoint_current",
        "acceptance_json",
        "TEXT NOT NULL DEFAULT '{}'",
    )?;
    ensure_text_column(
        connection,
        "SUBMIT_LOOP__review_current",
        "target_metadata_json",
        "TEXT NOT NULL DEFAULT '{}'",
    )?;
    let review_updated_at_added = ensure_text_column(
        connection,
        "SUBMIT_LOOP__review_current",
        "updated_at",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    if review_updated_at_added || review_current_updated_at_needs_backfill(connection)? {
        backfill_review_current_updated_at(connection)?;
    }
    ensure_text_column(
        connection,
        "SUBMIT_LOOP__loop_current",
        "resolved_role_selection_ref",
        "TEXT",
    )?;
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .context("failed to persist SQLite schema version")?;
    Ok(())
}

fn ensure_text_column(
    connection: &Connection,
    table: &str,
    column: &str,
    column_definition: &str,
) -> Result<bool> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut statement = connection.prepare(&pragma)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let existing: String = row.get(1)?;
        if existing == column {
            return Ok(false);
        }
    }
    connection.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {column_definition}"),
        [],
    )?;
    Ok(true)
}

fn review_current_updated_at_needs_backfill(connection: &Connection) -> Result<bool> {
    connection
        .query_row(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM SUBMIT_LOOP__review_current
                WHERE updated_at = ''
            )
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn backfill_review_current_updated_at(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        r#"
        SELECT loop_id, payload_json, recorded_at
        FROM CORE__events
        WHERE event_name IN (
            'SUBMIT_LOOP__review_round_opened',
            'SUBMIT_LOOP__checkpoint_review_round_recorded',
            'SUBMIT_LOOP__artifact_review_round_recorded'
        )
        ORDER BY event_id ASC
        "#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut review_updated_at = HashMap::new();
    for row in rows {
        let (loop_id, payload_json, recorded_at) = row?;
        let payload: Value =
            serde_json::from_str(&payload_json).context("failed to decode review event payload")?;
        let review_round_id = required_str(&payload, "review_round_id")?;
        review_updated_at.insert((loop_id, review_round_id.to_owned()), recorded_at);
    }

    let mut update = connection.prepare(
        r#"
        UPDATE SUBMIT_LOOP__review_current
        SET updated_at = ?3
        WHERE loop_id = ?1 AND review_round_id = ?2
        "#,
    )?;
    for ((loop_id, review_round_id), updated_at) in review_updated_at {
        update.execute(params![loop_id, review_round_id, updated_at])?;
    }
    Ok(())
}

pub(crate) fn store_json_content(
    transaction: &Transaction<'_>,
    content_kind: &str,
    payload: &Value,
) -> Result<String> {
    let serialized = serde_json::to_string(payload)?;
    let mut hasher = Sha256::new();
    hasher.update(content_kind.as_bytes());
    hasher.update([0]);
    hasher.update(serialized.as_bytes());
    let content_ref = format!("{:x}", hasher.finalize());
    let created_at = system::timestamp()?;

    transaction.execute(
        r#"
        INSERT INTO CORE__contents (content_ref, content_kind, payload_json, created_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(content_ref) DO NOTHING
        "#,
        params![content_ref, content_kind, serialized, created_at],
    )?;

    Ok(content_ref)
}

pub(crate) fn append_event(
    transaction: &Transaction<'_>,
    loop_id: &str,
    event_name: &str,
    payload: &Value,
) -> Result<i64> {
    let loop_seq: i64 = transaction.query_row(
        &format!(
            "SELECT COALESCE(MAX(loop_seq), 0) + 1 FROM {CORE_EVENT_TABLE} WHERE loop_id = ?1"
        ),
        [loop_id],
        |row| row.get(0),
    )?;
    let recorded_at = system::timestamp()?;

    transaction.execute(
        &format!(
            r#"
            INSERT INTO {CORE_EVENT_TABLE} (loop_id, loop_seq, event_name, payload_json, occurred_at, recorded_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#
        ),
        params![
            loop_id,
            loop_seq,
            event_name,
            serde_json::to_string(payload)?,
            recorded_at,
            recorded_at
        ],
    )?;

    Ok(transaction.last_insert_rowid())
}

pub(crate) fn rebuild_all_projections(transaction: &Transaction<'_>) -> Result<()> {
    // `*_current` tables are derived-only caches; a full rebuild must clear them and replay `CORE__events`.
    transaction.execute("DELETE FROM CORE__invocation_current", [])?;
    transaction.execute("DELETE FROM CORE__capability_current", [])?;
    transaction.execute("DELETE FROM CORE__result_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__loop_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__plan_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__checkpoint_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__review_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__worktree_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__caller_finalize_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__timeout_extension_current", [])?;
    transaction.execute("DELETE FROM SUBMIT_LOOP__commit_current", [])?;

    let mut statement = transaction.prepare(
        "SELECT event_id, loop_id, event_name, payload_json, recorded_at FROM CORE__events ORDER BY event_id ASC",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let event_id: i64 = row.get(0)?;
        let loop_id: String = row.get(1)?;
        let event_name: String = row.get(2)?;
        let payload_json: String = row.get(3)?;
        let recorded_at: String = row.get(4)?;
        let payload: Value = serde_json::from_str(&payload_json)
            .with_context(|| format!("failed to decode payload for event {event_id}"))?;
        replay_event_record(
            transaction,
            event_id,
            &loop_id,
            event_name.as_str(),
            &payload,
            &recorded_at,
        )?;
    }

    Ok(())
}

fn clear_loop_projections(transaction: &Transaction<'_>, loop_id: &str) -> Result<()> {
    transaction.execute(
        "DELETE FROM CORE__capability_current WHERE invocation_id IN (SELECT invocation_id FROM CORE__invocation_current WHERE loop_id = ?1)",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM CORE__invocation_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM CORE__result_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__loop_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__plan_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__review_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__worktree_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__caller_finalize_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__timeout_extension_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__commit_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    Ok(())
}

pub(crate) fn rebuild_single_loop_projections(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<()> {
    // Per-loop rebuild uses the same derived-state rule, but scopes the reset to one loop's current rows.
    clear_loop_projections(transaction, loop_id)?;
    let mut statement = transaction.prepare(
        "SELECT event_id, event_name, payload_json, recorded_at FROM CORE__events WHERE loop_id = ?1 ORDER BY event_id ASC",
    )?;
    let mut rows = statement.query([loop_id])?;
    while let Some(row) = rows.next()? {
        let event_id: i64 = row.get(0)?;
        let event_name: String = row.get(1)?;
        let payload_json: String = row.get(2)?;
        let recorded_at: String = row.get(3)?;
        let payload: Value = serde_json::from_str(&payload_json)
            .with_context(|| format!("failed to decode payload for event {event_id}"))?;
        replay_event_record(
            transaction,
            event_id,
            loop_id,
            event_name.as_str(),
            &payload,
            &recorded_at,
        )?;
    }
    Ok(())
}

fn replay_event_record(
    transaction: &Transaction<'_>,
    event_id: i64,
    loop_id: &str,
    event_name: &str,
    payload: &Value,
    recorded_at: &str,
) -> Result<()> {
    match event_name {
        "SUBMIT_LOOP__loop_opened" => {
            replay_loop_opened(transaction, event_id, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__loop_failed" => {
            replay_loop_failed(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__loop_succeeded" => {
            replay_loop_succeeded(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__caller_finalize_handed_off" => {
            replay_caller_finalize_handed_off(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__caller_finalize_started" => {
            replay_caller_finalize_started(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__caller_finalize_blocked" => {
            replay_caller_finalize_blocked(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__caller_integration_recorded" => {}
        "SUBMIT_LOOP__accepted_commits_integrated" => {
            replay_accepted_commits_integrated(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__worktree_created" | "SUBMIT_LOOP__worktree_prepared" => {
            replay_worktree_prepared(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__worktree_create_failed" | "SUBMIT_LOOP__worktree_prepare_failed" => {
            replay_worktree_prepare_failed(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__worktree_deleted" => {
            replay_worktree_deleted(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__worktree_cleanup_warning" => {
            replay_worktree_cleanup_warning(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__review_round_opened" => {
            replay_review_round_opened(transaction, recorded_at, payload, loop_id)?
        }
        "SUBMIT_LOOP__checkpoint_review_round_recorded"
        | "SUBMIT_LOOP__artifact_review_round_recorded" => {
            replay_review_round_event(transaction, recorded_at, event_name, payload, loop_id)?
        }
        "SUBMIT_LOOP__checkpoint_review_submitted"
        | "SUBMIT_LOOP__artifact_review_submitted"
        | "SUBMIT_LOOP__review_blocked_recorded" => {}
        "CORE__invocation_opened" => {
            replay_invocation_opened(transaction, loop_id, recorded_at, payload)?
        }
        "CORE__terminal_api_called" => {
            replay_terminal_api_called(transaction, recorded_at, payload)?
        }
        "CORE__result_materialized" => replay_result_materialized(transaction, payload, loop_id)?,
        "SUBMIT_LOOP__plan_submitted" => {
            replay_plan_submitted(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__plan_accepted" => {
            replay_plan_accepted(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__plan_rejected" => replay_plan_rejected(transaction, loop_id, recorded_at)?,
        "SUBMIT_LOOP__candidate_commit_submitted" => {
            replay_candidate_commit_submitted(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__artifact_accepted" => {
            replay_artifact_accepted(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__artifact_rejected" => {
            replay_artifact_rejected(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__accepted_commit_recorded" => {
            replay_accepted_commit_recorded(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__candidate_commit_revoked" => {
            replay_candidate_commit_revoked(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__timeout_extension_requested" => {
            replay_timeout_extension_requested(transaction, loop_id, recorded_at, payload)?
        }
        "SUBMIT_LOOP__attempt_consumed"
        | "SUBMIT_LOOP__worker_blocked_accepted"
        | "CORE__dispatch_retry_scheduled" => {}
        "CORE__request_materialized"
        | "CORE__dispatch_started"
        | "CORE__response_received"
        | "CORE__invocation_failed" => {
            replay_invocation_status_event(transaction, recorded_at, event_name, payload)?
        }
        other => bail!("unsupported event while rebuilding projections: {other}"),
    }
    Ok(())
}

fn replay_loop_opened(
    transaction: &Transaction<'_>,
    event_id: i64,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    let status = required_str(payload, "status")?;
    let worktree_path = required_str(payload, "worktree_path")?;
    let worktree_branch = required_str(payload, "worktree_branch")?;
    let worktree_label = required_str(payload, "worktree_label")?;
    let base_commit_sha = required_str(payload, "base_commit_sha")?;
    let loop_input_ref = required_str(payload, "loop_input_ref")?;
    let resolved_role_selection_ref = payload
        .get("resolved_role_selection_ref")
        .and_then(Value::as_str)
        .unwrap_or(loop_input_ref);
    let coordinator_role_ref = required_str(payload, "coordinator_role_ref")?;

    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__loop_current (
            loop_id,
            phase,
            status,
            worktree_path,
            worktree_branch,
            worktree_label,
            base_commit_sha,
            loop_input_ref,
            resolved_role_selection_ref,
            coordinator_role_ref,
            latest_event_id,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(loop_id) DO UPDATE SET
            phase = excluded.phase,
            status = excluded.status,
            worktree_path = excluded.worktree_path,
            worktree_branch = excluded.worktree_branch,
            worktree_label = excluded.worktree_label,
            base_commit_sha = excluded.base_commit_sha,
            loop_input_ref = excluded.loop_input_ref,
            resolved_role_selection_ref = excluded.resolved_role_selection_ref,
            coordinator_role_ref = excluded.coordinator_role_ref,
            latest_event_id = excluded.latest_event_id,
            updated_at = excluded.updated_at
        "#,
        params![
            loop_id,
            phase,
            status,
            worktree_path,
            worktree_branch,
            worktree_label,
            base_commit_sha,
            loop_input_ref,
            resolved_role_selection_ref,
            coordinator_role_ref,
            event_id,
            recorded_at
        ],
    )?;

    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__plan_current (
            loop_id,
            latest_submitted_plan_revision,
            current_executable_plan_revision,
            updated_at
        ) VALUES (?1, NULL, NULL, ?2)
        ON CONFLICT(loop_id) DO UPDATE SET updated_at = excluded.updated_at
        "#,
        params![loop_id, recorded_at],
    )?;

    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__worktree_current (
            loop_id,
            path,
            branch,
            label,
            lifecycle,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, 'reserved', ?5)
        ON CONFLICT(loop_id) DO UPDATE SET
            path = excluded.path,
            branch = excluded.branch,
            label = excluded.label,
            lifecycle = excluded.lifecycle,
            updated_at = excluded.updated_at
        "#,
        params![
            loop_id,
            worktree_path,
            worktree_branch,
            worktree_label,
            recorded_at
        ],
    )?;

    Ok(())
}

fn replay_loop_failed(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let failure_cause_type = required_str(payload, "failure_cause_type")?;
    let summary = required_str(payload, "summary")?;
    let phase_at_failure = required_str(payload, "phase_at_failure")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2,
            status = 'failed',
            failure_cause_type = ?3,
            failure_summary = ?4,
            updated_at = ?5
        WHERE loop_id = ?1
        "#,
        params![
            loop_id,
            phase_at_failure,
            failure_cause_type,
            summary,
            recorded_at
        ],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__caller_finalize_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    Ok(())
}

fn replay_loop_succeeded(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2,
            status = 'succeeded',
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, phase, recorded_at],
    )?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__caller_finalize_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    Ok(())
}

fn replay_accepted_commits_integrated(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2,
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, phase, recorded_at],
    )?;
    Ok(())
}

fn replay_caller_finalize_handed_off(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2,
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, phase, recorded_at],
    )?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__caller_finalize_current (
            loop_id,
            status,
            block_context_ref,
            updated_at
        ) VALUES (?1, 'ready', NULL, ?2)
        ON CONFLICT(loop_id) DO UPDATE SET
            status = excluded.status,
            block_context_ref = excluded.block_context_ref,
            updated_at = excluded.updated_at
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_caller_finalize_started(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2,
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, phase, recorded_at],
    )?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__caller_finalize_current
        SET status = 'active',
            block_context_ref = NULL,
            updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_caller_finalize_blocked(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    let block_context_ref = required_str(payload, "block_context_ref")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2,
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, phase, recorded_at],
    )?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__caller_finalize_current
        SET status = 'blocked',
            block_context_ref = ?2,
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, block_context_ref, recorded_at],
    )?;
    Ok(())
}

fn replay_worktree_prepared(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let phase = required_str(payload, "phase")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2, updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, phase, recorded_at],
    )?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__worktree_current
        SET lifecycle = 'prepared', updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_worktree_prepare_failed(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    _payload: &Value,
    loop_id: &str,
) -> Result<()> {
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__worktree_current
        SET lifecycle = 'prepare_failed', updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_worktree_deleted(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    _payload: &Value,
    loop_id: &str,
) -> Result<()> {
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__worktree_current
        SET lifecycle = 'deleted', updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_worktree_cleanup_warning(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    _payload: &Value,
    loop_id: &str,
) -> Result<()> {
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__worktree_current
        SET lifecycle = 'cleanup_warning', updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_review_round_opened(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let review_round_id = required_str(payload, "review_round_id")?;
    let review_kind = required_str(payload, "review_kind")?;
    let round_status = required_str(payload, "round_status")?;
    let target_type = required_str(payload, "target_type")?;
    let target_ref = required_str(payload, "target_ref")?;
    let target_metadata_json = serde_json::to_string(
        &payload
            .get("target_metadata")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )?;
    let slot_state_json = serde_json::to_string(
        payload
            .get("slot_state")
            .ok_or_else(|| anyhow!("missing slot_state"))?,
    )?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__review_current (
            loop_id,
            review_round_id,
            review_kind,
            round_status,
            target_type,
            target_ref,
            target_metadata_json,
            slot_state_json,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(loop_id, review_round_id) DO UPDATE SET
            review_kind = excluded.review_kind,
            round_status = excluded.round_status,
            target_type = excluded.target_type,
            target_ref = excluded.target_ref,
            target_metadata_json = excluded.target_metadata_json,
            slot_state_json = excluded.slot_state_json,
            updated_at = excluded.updated_at
        "#,
        params![
            loop_id,
            review_round_id,
            review_kind,
            round_status,
            target_type,
            target_ref,
            target_metadata_json,
            slot_state_json,
            recorded_at
        ],
    )?;
    let loop_phase = match review_kind {
        "checkpoint" => "checkpoint_review",
        "artifact" => "artifact_review",
        _ => "review",
    };
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = ?2, updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, loop_phase, recorded_at],
    )?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__plan_current
        SET updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_review_round_event(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    _event_name: &str,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let review_round_id = required_str(payload, "review_round_id")?;
    let review_kind = required_str(payload, "review_kind")?;
    let round_status = required_str(payload, "round_status")?;
    let target_type = required_str(payload, "target_type")?;
    let target_ref = required_str(payload, "target_ref")?;
    let target_metadata_json = serde_json::to_string(
        &payload
            .get("target_metadata")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )?;
    let slot_state_json = serde_json::to_string(
        payload
            .get("slot_state")
            .ok_or_else(|| anyhow!("missing slot_state"))?,
    )?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__review_current (
            loop_id,
            review_round_id,
            review_kind,
            round_status,
            target_type,
            target_ref,
            target_metadata_json,
            slot_state_json,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(loop_id, review_round_id) DO UPDATE SET
            review_kind = excluded.review_kind,
            round_status = excluded.round_status,
            target_type = excluded.target_type,
            target_ref = excluded.target_ref,
            target_metadata_json = excluded.target_metadata_json,
            slot_state_json = excluded.slot_state_json,
            updated_at = excluded.updated_at
        "#,
        params![
            loop_id,
            review_round_id,
            review_kind,
            round_status,
            target_type,
            target_ref,
            target_metadata_json,
            slot_state_json,
            recorded_at
        ],
    )?;
    Ok(())
}

fn replay_invocation_opened(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let invocation_id = required_str(payload, "invocation_id")?;
    let invocation_role = required_str(payload, "invocation_role")?;
    let stage = required_str(payload, "stage")?;
    let status = required_str(payload, "status")?;
    let token = required_str(payload, "token")?;
    let role_definition_ref = required_str(payload, "role_definition_ref")?;
    let executor_config_ref = required_str(payload, "resolved_executor_config_ref")?;
    let invocation_context_ref = required_str(payload, "invocation_context_ref")?;
    let allowed_terminal_apis_json = serde_json::to_string(
        payload
            .get("allowed_terminal_apis")
            .ok_or_else(|| anyhow!("missing allowed_terminal_apis"))?,
    )?;
    let review_round_id = payload
        .get("review_round_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let review_slot_id = payload
        .get("review_slot_id")
        .and_then(Value::as_str)
        .map(str::to_owned);

    transaction.execute(
        r#"
        INSERT INTO CORE__invocation_current (
            invocation_id,
            loop_id,
            invocation_role,
            stage,
            status,
            token,
            accepted_api,
            accepted_submission_id,
            role_definition_ref,
            executor_config_ref,
            invocation_context_ref,
            review_round_id,
            review_slot_id,
            allowed_terminal_apis_json,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(invocation_id) DO UPDATE SET
            status = excluded.status,
            token = excluded.token,
            role_definition_ref = excluded.role_definition_ref,
            executor_config_ref = excluded.executor_config_ref,
            invocation_context_ref = excluded.invocation_context_ref,
            review_round_id = excluded.review_round_id,
            review_slot_id = excluded.review_slot_id,
            allowed_terminal_apis_json = excluded.allowed_terminal_apis_json,
            updated_at = excluded.updated_at
        "#,
        params![
            invocation_id,
            loop_id,
            invocation_role,
            stage,
            status,
            token,
            role_definition_ref,
            executor_config_ref,
            invocation_context_ref,
            review_round_id,
            review_slot_id,
            allowed_terminal_apis_json,
            recorded_at
        ],
    )?;

    transaction.execute(
        r#"
        INSERT INTO CORE__capability_current (
            invocation_id,
            token,
            token_state,
            accepted_api,
            accepted_submission_id,
            updated_at
        ) VALUES (?1, ?2, 'available', NULL, NULL, ?3)
        ON CONFLICT(invocation_id) DO UPDATE SET
            token = excluded.token,
            token_state = excluded.token_state,
            updated_at = excluded.updated_at
        "#,
        params![invocation_id, token, recorded_at],
    )?;

    Ok(())
}

fn replay_invocation_status_event(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    event_name: &str,
    payload: &Value,
) -> Result<()> {
    let invocation_id = required_str(payload, "invocation_id")?;
    let mut status = match event_name {
        "CORE__request_materialized" | "CORE__dispatch_started" | "CORE__response_received" => {
            "running"
        }
        "CORE__invocation_failed" => "failed",
        _ => "opened",
    }
    .to_owned();
    if event_name == "CORE__response_received" {
        let accepted_api = transaction.query_row(
            "SELECT accepted_api FROM CORE__invocation_current WHERE invocation_id = ?1",
            [invocation_id],
            |row| row.get::<_, Option<String>>(0),
        )?;
        if accepted_api.is_some() {
            status = "accepted".to_owned();
        }
    }
    transaction.execute(
        r#"
        UPDATE CORE__invocation_current
        SET status = ?2, updated_at = ?3
        WHERE invocation_id = ?1
        "#,
        params![invocation_id, status, recorded_at],
    )?;
    Ok(())
}

fn replay_terminal_api_called(
    transaction: &Transaction<'_>,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let invocation_id = required_str(payload, "invocation_id")?;
    let submission_id = required_str(payload, "submission_id")?;
    let api_name = required_str(payload, "api_name")?;

    transaction.execute(
        r#"
        UPDATE CORE__invocation_current
        SET status = 'accepted',
            accepted_api = ?2,
            accepted_submission_id = ?3,
            updated_at = ?4
        WHERE invocation_id = ?1
        "#,
        params![invocation_id, api_name, submission_id, recorded_at],
    )?;
    transaction.execute(
        r#"
        UPDATE CORE__capability_current
        SET token_state = 'consumed',
            accepted_api = ?2,
            accepted_submission_id = ?3,
            updated_at = ?4
        WHERE invocation_id = ?1
        "#,
        params![invocation_id, api_name, submission_id, recorded_at],
    )?;

    Ok(())
}

fn replay_plan_submitted(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let plan_revision = payload
        .get("plan_revision")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing integer field plan_revision"))?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__plan_current (
            loop_id,
            latest_submitted_plan_revision,
            current_executable_plan_revision,
            updated_at
        ) VALUES (?1, ?2, NULL, ?3)
        ON CONFLICT(loop_id) DO UPDATE SET
            latest_submitted_plan_revision = excluded.latest_submitted_plan_revision,
            updated_at = excluded.updated_at
        "#,
        params![loop_id, plan_revision, recorded_at],
    )?;
    Ok(())
}

fn replay_timeout_extension_requested(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let invocation_id = required_str(payload, "invocation_id")?;
    let request_content_ref = required_str(payload, "request_content_ref")?;
    let request_payload_json: String = transaction.query_row(
        "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
        [request_content_ref],
        |row| row.get(0),
    )?;
    let request_payload: Value = serde_json::from_str(&request_payload_json)
        .context("failed to decode timeout extension request payload")?;
    let requested_timeout_sec = request_payload
        .get("requested_timeout_sec")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("timeout extension request is missing requested_timeout_sec"))?;
    let progress_summary = required_str(&request_payload, "progress_summary")?;
    let rationale = required_str(&request_payload, "rationale")?;

    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__timeout_extension_current (
            invocation_id,
            loop_id,
            latest_request_content_ref,
            requested_timeout_sec,
            progress_summary,
            rationale,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(invocation_id) DO UPDATE SET
            latest_request_content_ref = excluded.latest_request_content_ref,
            requested_timeout_sec = excluded.requested_timeout_sec,
            progress_summary = excluded.progress_summary,
            rationale = excluded.rationale,
            updated_at = excluded.updated_at
        "#,
        params![
            invocation_id,
            loop_id,
            request_content_ref,
            requested_timeout_sec,
            progress_summary,
            rationale,
            recorded_at
        ],
    )?;

    Ok(())
}

fn replay_plan_accepted(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let plan_revision = payload
        .get("plan_revision")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing integer field plan_revision"))?;
    let checkpoints = payload
        .get("checkpoints")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing checkpoints"))?;
    transaction.execute(
        "DELETE FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1",
        [loop_id],
    )?;
    for checkpoint in checkpoints {
        let checkpoint_id = required_str(checkpoint, "checkpoint_id")?;
        let sequence_index = checkpoint
            .get("sequence_index")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("missing integer field sequence_index"))?;
        let title = required_str(checkpoint, "title")?;
        let kind = checkpoint
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| !kind.trim().is_empty())
            .unwrap_or("artifact");
        let deliverables = checkpoint
            .get("deliverables")
            .cloned()
            .filter(|deliverables| deliverables.is_array())
            .unwrap_or_else(|| json!([]));
        let acceptance = required_checkpoint_acceptance(checkpoint)
            .with_context(|| format!("checkpoint {} has invalid acceptance", checkpoint_id))?;
        let revision = checkpoint
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let deliverables_json = serde_json::to_string(&deliverables)?;
        let acceptance_json = serde_json::to_string(&acceptance)?;
        transaction.execute(
            r#"
            INSERT INTO SUBMIT_LOOP__checkpoint_current (
                loop_id,
                checkpoint_id,
                sequence_index,
                title,
                kind,
                deliverables_json,
                acceptance_json,
                revision,
                execution_state,
                candidate_commit_sha,
                accepted_commit_sha,
                active
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', NULL, NULL, 1)
            "#,
            params![
                loop_id,
                checkpoint_id,
                sequence_index,
                title,
                kind,
                deliverables_json,
                acceptance_json,
                revision
            ],
        )?;
    }
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__plan_current
        SET current_executable_plan_revision = ?2,
            updated_at = ?3
        WHERE loop_id = ?1
        "#,
        params![loop_id, plan_revision, recorded_at],
    )?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = 'artifact', updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn required_checkpoint_acceptance(checkpoint: &Value) -> Result<Value> {
    let acceptance = checkpoint
        .get("acceptance")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("missing object field acceptance"))?;
    let verification_steps = acceptance
        .get("verification_steps")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("missing array field acceptance.verification_steps"))?;
    let expected_outcomes = acceptance
        .get("expected_outcomes")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("missing array field acceptance.expected_outcomes"))?;
    Ok(json!({
        "verification_steps": verification_steps,
        "expected_outcomes": expected_outcomes,
    }))
}

fn replay_plan_rejected(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
) -> Result<()> {
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__loop_current
        SET phase = 'planning', updated_at = ?2
        WHERE loop_id = ?1
        "#,
        params![loop_id, recorded_at],
    )?;
    Ok(())
}

fn replay_candidate_commit_submitted(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let checkpoint_id = required_str(payload, "checkpoint_id")?;
    let candidate_commit_sha = required_str(payload, "candidate_commit_sha")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__checkpoint_current
        SET candidate_commit_sha = ?3,
            execution_state = 'candidate_review',
            active = 1
        WHERE loop_id = ?1 AND checkpoint_id = ?2
        "#,
        params![loop_id, checkpoint_id, candidate_commit_sha],
    )?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__commit_current (
            loop_id,
            checkpoint_id,
            commit_sha,
            lifecycle,
            updated_at
        ) VALUES (?1, ?2, ?3, 'candidate', ?4)
        ON CONFLICT(loop_id, checkpoint_id, commit_sha) DO UPDATE SET
            lifecycle = excluded.lifecycle,
            updated_at = excluded.updated_at
        "#,
        params![loop_id, checkpoint_id, candidate_commit_sha, recorded_at],
    )?;
    Ok(())
}

fn replay_artifact_accepted(
    transaction: &Transaction<'_>,
    loop_id: &str,
    _recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let checkpoint_id = required_str(payload, "checkpoint_id")?;
    let candidate_commit_sha = required_str(payload, "candidate_commit_sha")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__checkpoint_current
        SET accepted_commit_sha = ?3,
            candidate_commit_sha = NULL,
            execution_state = 'accepted',
            active = 1
        WHERE loop_id = ?1 AND checkpoint_id = ?2
        "#,
        params![loop_id, checkpoint_id, candidate_commit_sha],
    )?;
    Ok(())
}

fn replay_artifact_rejected(
    transaction: &Transaction<'_>,
    loop_id: &str,
    _recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let checkpoint_id = required_str(payload, "checkpoint_id")?;
    transaction.execute(
        r#"
        UPDATE SUBMIT_LOOP__checkpoint_current
        SET candidate_commit_sha = NULL,
            execution_state = 'pending',
            active = 1
        WHERE loop_id = ?1 AND checkpoint_id = ?2
        "#,
        params![loop_id, checkpoint_id],
    )?;
    Ok(())
}

fn replay_accepted_commit_recorded(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let checkpoint_id = required_str(payload, "checkpoint_id")?;
    let commit_sha = required_str(payload, "commit_sha")?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__commit_current (
            loop_id,
            checkpoint_id,
            commit_sha,
            lifecycle,
            updated_at
        ) VALUES (?1, ?2, ?3, 'accepted', ?4)
        ON CONFLICT(loop_id, checkpoint_id, commit_sha) DO UPDATE SET
            lifecycle = excluded.lifecycle,
            updated_at = excluded.updated_at
        "#,
        params![loop_id, checkpoint_id, commit_sha, recorded_at],
    )?;
    Ok(())
}

fn replay_candidate_commit_revoked(
    transaction: &Transaction<'_>,
    loop_id: &str,
    recorded_at: &str,
    payload: &Value,
) -> Result<()> {
    let checkpoint_id = required_str(payload, "checkpoint_id")?;
    let commit_sha = required_str(payload, "commit_sha")?;
    transaction.execute(
        r#"
        INSERT INTO SUBMIT_LOOP__commit_current (
            loop_id,
            checkpoint_id,
            commit_sha,
            lifecycle,
            updated_at
        ) VALUES (?1, ?2, ?3, 'revoked', ?4)
        ON CONFLICT(loop_id, checkpoint_id, commit_sha) DO UPDATE SET
            lifecycle = excluded.lifecycle,
            updated_at = excluded.updated_at
        "#,
        params![loop_id, checkpoint_id, commit_sha, recorded_at],
    )?;
    Ok(())
}

fn replay_result_materialized(
    transaction: &Transaction<'_>,
    payload: &Value,
    loop_id: &str,
) -> Result<()> {
    let status = required_str(payload, "status")?;
    let result_ref = required_str(payload, "result_ref")?;
    // The event payload owns `generated_at`; replay must not replace it with event-record time.
    let generated_at = required_str(payload, "generated_at")?;
    transaction.execute(
        r#"
        INSERT INTO CORE__result_current (loop_id, status, result_ref, generated_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(loop_id) DO UPDATE SET
            status = excluded.status,
            result_ref = excluded.result_ref,
            generated_at = excluded.generated_at
        "#,
        params![loop_id, status, result_ref, generated_at],
    )?;
    Ok(())
}

pub(crate) fn append_transcript_segment(
    transaction: &Transaction<'_>,
    invocation_id: &str,
    speaker: &str,
    segment_kind: &str,
    summary: Option<&str>,
    content_ref: &str,
) -> Result<()> {
    let ordinal: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM CORE__transcript_segments WHERE invocation_id = ?1",
        [invocation_id],
        |row| row.get(0),
    )?;
    transaction.execute(
        r#"
        INSERT INTO CORE__transcript_segments (
            invocation_id,
            ordinal,
            speaker,
            segment_kind,
            summary,
            content_ref,
            occurred_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            invocation_id,
            ordinal,
            speaker,
            segment_kind,
            summary,
            content_ref,
            system::timestamp()?
        ],
    )?;
    Ok(())
}
