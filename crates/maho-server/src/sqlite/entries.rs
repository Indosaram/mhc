use super::values::ValueError;
use maho_agent::harness::session::{Entry, EntryStructure, EntryScan, ScanOrder, UsageRow, UsageScan};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn insert_entry(db: &Connection, session: &str, entry: &Entry) -> Result<(), ValueError> {
    let mut payload = serde_json::to_value(&entry.kind)?;
    let kind = payload["type"].as_str().unwrap_or_default().to_owned();
    let custom = entry.custom_type();
    let fields = payload
        .as_object_mut()
        .ok_or_else(|| ValueError::Options("Invalid entry payload".into()))?;
    fields.remove("type");
    fields.remove("customType");
    db.prepare_cached("INSERT INTO entries (session_id,id,parent_id,seq,type,custom_type,timestamp,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")?.execute(params![session,entry.id,entry.parent_id,entry.seq,kind,custom,entry.timestamp,serde_json::to_string(&payload)?])?;
    Ok(())
}
pub fn read_entry_structures(db: &Connection,session: &str,ids: &[String]) -> Result<Vec<EntryStructure>,ValueError> {
    let mut statement = db.prepare_cached("SELECT id,parent_id,seq,type,custom_type,timestamp FROM entries WHERE session_id=?1 AND id=?2")?;
    let mut output = Vec::new();
    for id in ids {
        let mut rows = statement.query(params![session,id])?;
        if let Some(row) = rows.next()? {
            output.push(EntryStructure {id:row.get(0)?,parent_id:row.get(1)?,seq:row.get(2)?,entry_type:serde_json::from_value(json!(row.get::<_,String>(3)?))?,custom_type:row.get(4)?,timestamp:row.get(5)?});
        }
    }
    Ok(output)
}
pub fn scan_entries(
    db: &Connection,
    session: &str,
    query: &EntryScan,
) -> Result<Vec<Entry>, ValueError> {
    let kind = query
        .entry_type
        .map(serde_json::to_value)
        .transpose()?
        .and_then(|v| v.as_str().map(str::to_owned));
    let order = if query.order == Some(ScanOrder::Desc) {
        "DESC"
    } else {
        "ASC"
    };
    let limit = query
        .limit
        .map(i64::try_from)
        .transpose()
        .map_err(|e| ValueError::Options(e.to_string()))?
        .unwrap_or(-1);
    let mut statement=db.prepare(&format!("SELECT id,parent_id,seq,type,custom_type,timestamp,payload FROM entries WHERE session_id=?1 AND (?2 IS NULL OR type=?2) AND (?3 IS NULL OR custom_type=?3) AND (?4 IS NULL OR seq>=?4) AND (?5 IS NULL OR seq<=?5) ORDER BY seq {order} LIMIT ?6"))?;
    let rows = statement.query_map(
        params![
            session,
            kind,
            query.custom_type,
            query.from_seq,
            query.to_seq,
            limit
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        },
    )?;
    rows.map(|row| {
        let (id, parent, seq, kind, custom, timestamp, payload) = row?;
        let mut entry: Value = serde_json::from_str(&payload)?;
        entry["id"] = json!(id);
        entry["parentId"] = json!(parent);
        entry["seq"] = json!(seq);
        entry["timestamp"] = json!(timestamp);
        entry["type"] = json!(kind);
        if kind == "custom" {
            entry["customType"] = json!(custom.ok_or_else(|| ValueError::Options(format!(
                "Custom entry {id} is missing custom_type"
            )))?);
        }
        let mut decoded: Entry = serde_json::from_value(entry.clone())?;
        match &mut decoded.kind {
            maho_agent::harness::session::EntryKind::Custom { data, .. } => {
                *data = entry.get("data").cloned()
            }
            maho_agent::harness::session::EntryKind::Compaction { details, .. }
            | maho_agent::harness::session::EntryKind::BranchSummary { details, .. } => {
                *details = entry.get("details").cloned()
            }
            maho_agent::harness::session::EntryKind::Message { .. } => {}
        }
        Ok(decoded)
    })
    .collect()
}
pub fn read_entries(
    db: &Connection,
    session: &str,
    ids: &[String],
) -> Result<Vec<Entry>, ValueError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(scan_entries(db, session, &EntryScan::default())?
        .into_iter()
        .filter(|entry| ids.contains(&entry.id))
        .collect())
}
pub fn insert_usage(db: &Connection, session: &str, usage: &UsageRow) -> Result<(), ValueError> {
    db.prepare_cached("INSERT INTO usage_ledger (session_id,id,seq,entry_id,adjustment,usage,details) VALUES (?1,?2,?3,?4,?5,?6,?7)")?.execute(params![session,usage.id,usage.seq,usage.entry_id,usage.adjustment,serde_json::to_string(&usage.usage)?,usage.details.as_ref().map(serde_json::to_string).transpose()?])?;
    Ok(())
}
pub fn scan_usage(
    db: &Connection,
    session: &str,
    query: &UsageScan,
) -> Result<Vec<UsageRow>, ValueError> {
    let order = if query.order == Some(ScanOrder::Desc) {
        "DESC"
    } else {
        "ASC"
    };
    let limit = query
        .limit
        .map(i64::try_from)
        .transpose()
        .map_err(|e| ValueError::Options(e.to_string()))?
        .unwrap_or(-1);
    let mut statement=db.prepare(&format!("SELECT id,seq,entry_id,adjustment,usage,details FROM usage_ledger WHERE session_id=?1 AND (?2 IS NULL OR seq>=?2) AND (?3 IS NULL OR seq<=?3) ORDER BY seq {order} LIMIT ?4"))?;
    let rows = statement.query_map(
        params![session, query.from_seq, query.to_seq, limit],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )?;
    rows.map(|row| {
        let (id, seq, entry_id, adjustment, usage, details) = row?;
        Ok(UsageRow {
            id,
            seq,
            entry_id,
            adjustment,
            usage: serde_json::from_str(&usage)?,
            details: details.map(|v| serde_json::from_str(&v)).transpose()?,
        })
    })
    .collect()
}
