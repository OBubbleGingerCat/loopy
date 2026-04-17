use anyhow::Result;
use loopy_common_events::CORE_EVENT_TABLE;
use rusqlite::Transaction;
use serde_json::Value;

// Owns neutral event scan/replay plumbing without submit-loop table semantics.

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub event_id: i64,
    pub loop_id: String,
    pub event_name: String,
    pub payload: Value,
    pub recorded_at: String,
}

pub fn scan_all_event_records(transaction: &Transaction<'_>) -> Result<Vec<EventRecord>> {
    scan_event_records(transaction, None)
}

pub fn scan_loop_event_records(
    transaction: &Transaction<'_>,
    loop_id: &str,
) -> Result<Vec<EventRecord>> {
    scan_event_records(transaction, Some(loop_id))
}

pub fn replay_event_records<F>(records: Vec<EventRecord>, mut apply: F) -> Result<()>
where
    F: FnMut(&EventRecord) -> Result<()>,
{
    for record in &records {
        apply(record)?;
    }
    Ok(())
}

fn scan_event_records(
    transaction: &Transaction<'_>,
    loop_id: Option<&str>,
) -> Result<Vec<EventRecord>> {
    let sql = if loop_id.is_some() {
        format!(
            "SELECT event_id, loop_id, event_name, payload_json, recorded_at FROM {CORE_EVENT_TABLE} WHERE loop_id = ?1 ORDER BY event_id ASC"
        )
    } else {
        format!(
            "SELECT event_id, loop_id, event_name, payload_json, recorded_at FROM {CORE_EVENT_TABLE} ORDER BY event_id ASC"
        )
    };
    let mut statement = transaction.prepare(&sql)?;
    let mut rows = if let Some(loop_id) = loop_id {
        statement.query([loop_id])?
    } else {
        statement.query([])?
    };
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        records.push(EventRecord {
            event_id: row.get(0)?,
            loop_id: row.get(1)?,
            event_name: row.get(2)?,
            payload: serde_json::from_str(&row.get::<_, String>(3)?)?,
            recorded_at: row.get(4)?,
        });
    }
    Ok(records)
}
