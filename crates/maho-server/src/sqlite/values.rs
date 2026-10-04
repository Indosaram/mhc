use maho_agent::harness::session::{
    ListElement, ListOrder, ListReadOptions, StoredValue, Value, ValueList,
    resolve_list_read_options,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value as JsonValue;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValueError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Options(String),
}
pub fn set_scalar(
    db: &Connection,
    session: &str,
    address: &Value,
    seq: i64,
    value: &JsonValue,
) -> Result<(), ValueError> {
    db.execute("INSERT INTO scalar_values (session_id,namespace,key,seq,value) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(session_id,namespace,key) DO UPDATE SET seq=excluded.seq,value=excluded.value",params![session,address.namespace,address.key,seq,serde_json::to_string(value)?])?;
    Ok(())
}
pub fn delete_scalar(db: &Connection, session: &str, address: &Value) -> Result<(), ValueError> {
    db.execute(
        "DELETE FROM scalar_values WHERE session_id=?1 AND namespace=?2 AND key=?3",
        params![session, address.namespace, address.key],
    )?;
    Ok(())
}
pub fn read_scalar(
    db: &Connection,
    session: &str,
    address: &Value,
) -> Result<Option<StoredValue>, ValueError> {
    let row = db
        .query_row(
            "SELECT seq,value FROM scalar_values WHERE session_id=?1 AND namespace=?2 AND key=?3",
            params![session, address.namespace, address.key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|(seq, value)| {
        Ok(StoredValue {
            address: address.clone(),
            seq,
            value: serde_json::from_str(&value)?,
        })
    })
    .transpose()
}
pub fn read_all_scalars(db: &Connection, session: &str) -> Result<Vec<StoredValue>, ValueError> {
    let mut query = db.prepare(
        "SELECT namespace,key,seq,value FROM scalar_values WHERE session_id=?1 ORDER BY seq ASC",
    )?;
    let rows = query.query_map([session], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (namespace, key, seq, value) = row?;
        Ok(StoredValue {
            address: Value { namespace, key },
            seq,
            value: serde_json::from_str(&value)?,
        })
    })
    .collect()
}
fn next_prefix_boundary(prefix: &str) -> Option<String> {
    let mut characters = prefix.chars().collect::<Vec<_>>();
    while let Some(last) = characters.pop() {
        if last < '\u{10ffff}' {
            let next = if last == '\u{d7ff}' {
                '\u{e000}'
            } else {
                char::from_u32(u32::from(last) + 1)?
            };
            characters.push(next);
            return Some(characters.into_iter().collect());
        }
    }
    None
}
pub fn scan_scalars(
    db: &Connection,
    session: &str,
    prefix: &Value,
) -> Result<Vec<StoredValue>, ValueError> {
    let upper = next_prefix_boundary(&prefix.key);
    let mut statement=db.prepare("SELECT namespace,key,seq,value FROM scalar_values WHERE session_id=?1 AND namespace=?2 AND key>=?3 AND (?4 IS NULL OR key<?4) ORDER BY key ASC")?;
    let rows = statement.query_map(
        params![session, prefix.namespace, prefix.key, upper],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    rows.map(|row| {
        let (namespace, key, seq, value) = row?;
        Ok(StoredValue {
            address: Value { namespace, key },
            seq,
            value: serde_json::from_str(&value)?,
        })
    })
    .collect()
}
pub fn append_list(
    db: &Connection,
    session: &str,
    address: &ValueList,
    seq: i64,
    value: &JsonValue,
) -> Result<(), ValueError> {
    db.execute(
        "INSERT INTO list_values (session_id,namespace,key,seq,value) VALUES (?1,?2,?3,?4,?5)",
        params![
            session,
            address.namespace,
            address.key,
            seq,
            serde_json::to_string(value)?
        ],
    )?;
    Ok(())
}
pub fn delete_list(db: &Connection, session: &str, address: &ValueList) -> Result<(), ValueError> {
    db.execute(
        "DELETE FROM list_values WHERE session_id=?1 AND namespace=?2 AND key=?3",
        params![session, address.namespace, address.key],
    )?;
    Ok(())
}
pub fn read_list(
    db: &Connection,
    session: &str,
    address: &ValueList,
    options: Option<ListReadOptions>,
) -> Result<Vec<ListElement>, ValueError> {
    let options =
        resolve_list_read_options(options).map_err(|e| ValueError::Options(e.to_string()))?;
    let query = match options.order {
        ListOrder::Asc => {
            "SELECT seq,value FROM list_values WHERE session_id=?1 AND namespace=?2 AND key=?3 AND (?4 IS NULL OR seq>?4) ORDER BY seq ASC LIMIT ?5"
        }
        ListOrder::Desc => {
            "SELECT seq,value FROM list_values WHERE session_id=?1 AND namespace=?2 AND key=?3 AND (?4 IS NULL OR seq<?4) ORDER BY seq DESC LIMIT ?5"
        }
    };
    let mut query = db.prepare(query)?;
    let limit = i64::try_from(options.limit).map_err(|e| ValueError::Options(e.to_string()))?;
    let rows = query.query_map(
        params![
            session,
            address.namespace,
            address.key,
            options.cursor.map(|c| c.seq),
            limit
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )?;
    rows.map(|row| {
        let (seq, value) = row?;
        Ok(ListElement {
            seq,
            value: serde_json::from_str(&value)?,
        })
    })
    .collect()
}
