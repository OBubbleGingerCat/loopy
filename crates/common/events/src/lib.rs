use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

// Owns neutral event/content/transcript append and lookup helpers.

pub const CORE_EVENT_TABLE: &str = "CORE__events";

pub fn timestamp() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

pub fn store_json_content(
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
    let created_at = timestamp()?;

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

pub fn append_event(
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
    let recorded_at = timestamp()?;

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

pub fn append_transcript_segment(
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
            timestamp()?
        ],
    )?;
    Ok(())
}

pub fn load_json_content(transaction: &Transaction<'_>, content_ref: &str) -> Result<Value> {
    transaction
        .query_row(
            "SELECT payload_json FROM CORE__contents WHERE content_ref = ?1",
            [content_ref],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload_json| serde_json::from_str(&payload_json))
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("unknown content_ref {content_ref}"))
}
