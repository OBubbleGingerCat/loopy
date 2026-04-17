use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, TransactionBehavior};

// Owns neutral SQLite connection setup and transaction behavior only.

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn begin_immediate_transaction(connection: &mut Connection) -> Result<Transaction<'_>> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("failed to begin SQLite immediate transaction")
}

pub fn configure_write_connection(connection: &Connection) -> Result<()> {
    configure_common_connection_settings(connection)
}

pub fn configure_read_only_connection(connection: &Connection) -> Result<()> {
    configure_common_connection_settings(connection)
}

fn configure_common_connection_settings(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .context("failed to configure SQLite busy timeout")?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable SQLite foreign key enforcement")?;
    Ok(())
}
