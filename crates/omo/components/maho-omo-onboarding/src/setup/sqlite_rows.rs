//! Read rows from a SQLite database (upstream `omo-native/bin/lib/sqlite-rows.js`). Upstream works
//! around Bun's `node:sqlite` keeping a statement alive past `close()`; rusqlite finalizes each
//! statement when it drops, so a plain prepared query releases the file the same way.

use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Map, Value};
use std::path::Path;

/// Opens `path` read-only.
///
/// # Errors
/// The database cannot be opened.
pub fn open_read_only(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())
}

fn json_of(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
    }
}

/// Every row of `sql`, each a map keyed by `columns` in projection order.
///
/// # Errors
/// The query failed.
pub fn read_rows(
    database: &Connection,
    columns: &[&str],
    sql: &str,
) -> Result<Vec<Map<String, Value>>, String> {
    let mut statement = database.prepare(sql).map_err(|e| e.to_string())?;
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut record = Map::new();
        for (index, column) in columns.iter().enumerate() {
            let value = row.get_ref(index).map_err(|e| e.to_string())?;
            record.insert((*column).to_owned(), json_of(value));
        }
        out.push(record);
    }
    Ok(out)
}

/// First row of [`read_rows`].
///
/// # Errors
/// The query failed.
pub fn read_row(
    database: &Connection,
    columns: &[&str],
    sql: &str,
) -> Result<Option<Map<String, Value>>, String> {
    Ok(read_rows(database, columns, sql)?.into_iter().next())
}
