//! Identify two specific traversal programs of the full, bound legacy SELECT. EQP alone
//! doesn't give the direction of a scan; an index *name* alone doesn't give its keys.
use rusqlite::{Connection, Statement, params_from_iter, types::Value};

// Keep the row of this aggregate live while explaining and fetching. This pins the schema
// and data in one ordinary read snapshot; no BEGIN, nested transaction, or scoring lock.
pub(super) const ROOTS: &str = "SELECT
    max(CASE WHEN type='table' AND name='episodic_memory' AND NOT EXISTS
        (SELECT 1 FROM pragma_table_xinfo('episodic_memory','main') WHERE hidden!=0) THEN rootpage END),
    max(CASE WHEN sql='CREATE INDEX idx_em_timestamp ON episodic_memory(timestamp)' THEN rootpage END),
    max(CASE WHEN sql='CREATE INDEX idx_em_scope_imp ON episodic_memory(scope, importance) WHERE superseded_by IS NULL' THEN rootpage END)
    FROM main.sqlite_schema WHERE tbl_name='episodic_memory'";

pub(super) type Roots = [Option<i64>; 3];

pub(super) fn order(
    conn: &Connection,
    sql: &str,
    values: &[Value],
    roots: Roots,
    observe: &mut impl FnMut(&str, &Statement<'_>),
) -> rusqlite::Result<Option<&'static str>> {
    // The instruction names and range code generation below are implementation details,
    // not SQLite API. A dependency upgrade must requalify these recognizers explicitly.
    if rusqlite::version_number() != 3_054_001 {
        return Ok(None);
    }
    let mut stmt = conn.prepare(&format!("EXPLAIN {sql}"))?;
    let mut rows = stmt.query(params_from_iter(values))?;
    let mut walk = Vec::new();
    let mut casts = 0;
    let mut last_rowid = None;
    let mut supported = true;
    #[cfg(test)]
    let mut instructions = 0;
    while let Some(row) = rows.next()? {
        #[cfg(test)]
        {
            instructions += 1;
        }
        let op = row.get_ref(1)?.as_str()?;
        let p1: i64 = row.get(2)?;
        let p2: i64 = row.get(3)?;
        let p3: i64 = row.get(4)?;
        let p4: Option<&str> = row.get_ref(5)?.as_str_or_null()?;
        let step = match op {
            "OpenRead" if p1 == 0 && p3 == 0 && Some(p2) == roots[0] => "table",
            "ReopenIdx" if p1 == 1 && p3 == 0 && Some(p2) == roots[1] && p4 == Some("k(2,,)") => {
                "time"
            }
            "OpenRead" if p1 == 1 && p3 == 0 && Some(p2) == roots[2] && p4 == Some("k(3,,,)") => {
                "scope/importance"
            }
            "Rewind" | "SeekGE" | "SeekGT" | "IdxGT" | "Next" if p1 == 1 => op,
            "Gosub" | "ResultRow" => op,
            // Filters, projection, or the ephemeral RHS of CAST(rowid AS TEXT) IN (...).
            "Init" | "Transaction" | "Halt" | "Goto" | "Variable" | "Null" | "Integer"
            | "String8" | "Affinity" | "Column" | "RealAffinity" | "NotNull" | "IsNull" | "Eq"
            | "Ne" | "Le" | "Gt" | "DeferredSeek" | "IdxRowid" | "Rowid" | "RowSetTest"
            | "BeginSubrtn" | "Once" | "OpenEphemeral" | "MakeRecord" | "IdxInsert" | "NullRow"
            | "Return" | "NotFound" => "",
            "Cast" if last_rowid == Some(p1) && p2 == 66 => {
                casts += 1;
                ""
            }
            _ => {
                supported = false;
                break;
            }
        };
        // Both casts must actually be of the b-tree rowid (not a shadowing user column).
        last_rowid = match (op, p1) {
            ("IdxRowid", 1) | ("Rowid", 0) => Some(p2),
            _ => None,
        };
        if !step.is_empty() {
            walk.push(step.to_owned());
        }
    }
    drop(rows);
    #[cfg(test)]
    eprintln!("episodic EXPLAIN instructions inspected={instructions}");
    observe("explain", &stmt);
    if !supported || casts != 2 {
        return Ok(None);
    }
    // For this query the timestamp OR has two disjoint ranges: NULL equality, then
    // non-NULL through the upper bound. Both advance the same (timestamp,rowid) b-tree.
    // k(...,,) specifies ascending binary keys; all ties within an index use integer rowid.
    let time_ranges = [
        "table",
        "time",
        "SeekGE",
        "IdxGT",
        "Gosub",
        "Next",
        "time",
        "SeekGT",
        "IdxGT",
        "Gosub",
        "Next",
        "ResultRow",
    ];
    if walk == time_ranges {
        Ok(Some("m.timestamp COLLATE BINARY, m.rowid"))
    } else if walk == ["table", "scope/importance", "Rewind", "ResultRow", "Next"] {
        Ok(Some(
            "m.scope COLLATE BINARY, m.importance COLLATE BINARY, m.rowid",
        ))
    } else {
        Ok(None)
    }
}
