use super::values::ValueError;
use maho_agent::harness::session::SessionMetadata;
use rusqlite::{Connection, Row, params};

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub created_at: i64,
    pub parent_session_id: Option<String>,
    pub storage_version: u32,
    pub metadata: Option<String>,
    pub message_count: i64,
    pub usage_payload: String,
    pub next_seq: i64,
}
fn decode(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        created_at: row.get(1)?,
        parent_session_id: row.get(2)?,
        storage_version: row.get(3)?,
        metadata: row.get(4)?,
        message_count: row.get(5)?,
        usage_payload: row.get(6)?,
        next_seq: row.get(7)?,
    })
}
pub fn read_session(db: &Connection, id: &str) -> rusqlite::Result<SessionRow> {
    db.query_row("SELECT id,created_at,parent_session_id,storage_version,metadata,message_count,usage_payload,next_seq FROM sessions WHERE id=?1",[id],decode)
}
pub fn read_all_sessions(db: &Connection) -> rusqlite::Result<Vec<SessionRow>> {
    db.prepare("SELECT id,created_at,parent_session_id,storage_version,metadata,message_count,usage_payload,next_seq FROM sessions")?.query_map([],decode)?.collect()
}
pub fn has_session(db: &Connection, id: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
        [id],
        |row| row.get(0),
    )
}
pub fn metadata_from_row(
    row: SessionRow,
    current_version: u32,
) -> Result<SessionMetadata, ValueError> {
    if row.storage_version > current_version {
        return Err(ValueError::Options(format!(
            "SQLite session storage version {} is newer than {current_version}",
            row.storage_version
        )));
    }
    if row.storage_version < current_version {
        return Err(ValueError::Options(format!(
            "SQLite session storage version {} requires migrations",
            row.storage_version
        )));
    }
    Ok(SessionMetadata {
        id: row.id,
        created_at: row.created_at,
        storage_version: row.storage_version,
        parent_session_id: row.parent_session_id,
        cwd: None,
        legacy_parent_session_path: None,
    })
}
pub fn insert_session(
    db: &Connection,
    metadata: &SessionMetadata,
    next_seq: i64,
) -> rusqlite::Result<()> {
    db.execute("INSERT INTO sessions (id,created_at,parent_session_id,storage_version,metadata,message_count,usage_payload,next_seq) VALUES (?1,?2,?3,?4,NULL,0,?5,?6)",params![metadata.id,metadata.created_at,metadata.parent_session_id,metadata.storage_version,r#"{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}"#,next_seq])?;
    Ok(())
}
pub fn delete_session_rows(db: &Connection, id: &str) -> Result<(), ValueError> {
    for table in [
        "entries",
        "scalar_values",
        "list_values",
        "usage_ledger",
        "branch_entries",
        "branch_meta",
    ] {
        db.execute(&format!("DELETE FROM {table} WHERE session_id=?1"), [id])?;
    }
    let changes = db.execute("DELETE FROM sessions WHERE id=?1", [id])?;
    if changes != 1 {
        return Err(ValueError::Options(format!(
            "Expected to delete one SQLite session {id}, deleted {changes}"
        )));
    }
    Ok(())
}
pub fn advance_next_seq(db: &Connection, id: &str, next_seq: i64) -> Result<(), ValueError> {
    let changes = db.execute(
        "UPDATE sessions SET next_seq=?1 WHERE id=?2",
        params![next_seq, id],
    )?;
    if changes != 1 {
        return Err(ValueError::Options(format!(
            "Expected to update one SQLite session {id}, updated {changes}"
        )));
    }
    Ok(())
}
