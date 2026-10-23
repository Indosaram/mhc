use super::{entries, values::ValueError};
use maho_agent::harness::session::{BranchOrder, StorageBranchScan};
use maho_agent::harness::session::{Entry, EntryStructure};
use rusqlite::{Connection, OptionalExtension, params};

pub fn scan_branch(
    db: &Connection,
    session: &str,
    query: &StorageBranchScan,
) -> Result<Vec<Entry>, ValueError> {
    let ids = scan_branch_ids(db,session,query)?;
    let entries = entries::read_entries(db,session,&ids)?;
    let mut by_id = entries.into_iter().map(|entry|(entry.id.clone(),entry)).collect::<std::collections::BTreeMap<_,_>>();
    Ok(ids.into_iter().filter_map(|id|by_id.remove(&id)).collect())
}
pub fn scan_branch_structure(db: &Connection,session: &str,query: &StorageBranchScan) -> Result<Vec<EntryStructure>,ValueError> {
    entries::read_entry_structures(db,session,&scan_branch_ids(db,session,query)?)
}
fn scan_branch_ids(db: &Connection,session: &str,query: &StorageBranchScan) -> Result<Vec<String>,ValueError> {
    let (mut branch,mut upper)=db.query_row("SELECT b.branch_id,b.entry_seq FROM branch_entries b JOIN branch_meta m ON m.session_id=b.session_id AND m.branch_id=b.branch_id WHERE b.session_id=?1 AND b.entry_id=?2 AND ((m.base_seq IS NULL AND b.entry_seq>0) OR (m.base_seq IS NOT NULL AND b.entry_seq>m.base_seq)) AND b.entry_seq<=m.tip_seq ORDER BY m.tip_seq DESC,b.branch_id LIMIT 1",params![session,query.start],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?;
    let mut segments = Vec::new();
    loop {
        let (base, lower) = db.query_row(
            "SELECT base_branch_id,base_seq FROM branch_meta WHERE session_id=?1 AND branch_id=?2",
            params![session, branch],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            },
        )?;
        segments.push((branch, lower.unwrap_or(0), upper));
        if let Some(base) = base {
            branch = base;
            upper = lower.ok_or_else(|| {
                ValueError::Options("Branch has base branch without base_seq".into())
            })?;
        } else {
            break;
        }
    }
    let oldest = query.order == Some(BranchOrder::OldestFirst);
    if oldest {
        segments.reverse();
    }
    let entry_type = query
        .entry_type
        .map(serde_json::to_value)
        .transpose()?
        .and_then(|v| v.as_str().map(str::to_owned));
    let stop_type = query
        .stop_at_type
        .map(serde_json::to_value)
        .transpose()?
        .and_then(|v| v.as_str().map(str::to_owned));
    let mut output = Vec::new();
    let limit = query.limit.unwrap_or(usize::MAX);
    for (branch, lower, upper) in segments {
        if output.len() >= limit {
            break;
        }
        let aggregate = if oldest { "MIN" } else { "MAX" };
        let order = if oldest { "ASC" } else { "DESC" };
        // PERF-DIVERGENCE(senpi): upstream re-prepares this SQL on every segment; we use the connection's statement cache (the text has only a few variants). Rows are identical.
        let stop=db.prepare_cached(&format!("SELECT {aggregate}(entry_seq) FROM branch_entries WHERE session_id=?1 AND branch_id=?2 AND entry_seq>?3 AND entry_seq<=?4 AND ((?5 IS NOT NULL AND entry_type=?5) OR (?6 IS NOT NULL AND entry_id=?6))"))?.query_row(params![session,branch,lower,upper,stop_type,query.stop_at_id],|row|row.get::<_,Option<i64>>(0))?;
        let (lower, upper) = if let Some(stop) = stop {
            if oldest {
                (lower, stop)
            } else {
                (lower.max(stop - 1), upper)
            }
        } else {
            (lower, upper)
        };
        let cursor = query.cursor.map(|c| c.seq);
        let remaining = i64::try_from(limit - output.len()).unwrap_or(i64::MAX);
        let cursor_filter = if oldest {
            "b.entry_seq>?8"
        } else {
            "b.entry_seq<?8"
        };
        // PERF-DIVERGENCE(senpi): upstream re-prepares this SQL per segment; we reuse the cached statement (a handful of text variants). Rows are identical.
        let mut statement=db.prepare_cached(&format!("SELECT b.entry_id FROM branch_entries b CROSS JOIN entries e ON e.session_id=b.session_id AND e.id=b.entry_id WHERE b.session_id=?1 AND b.branch_id=?2 AND b.entry_seq>?3 AND b.entry_seq<=?4 AND (?5 IS NULL OR b.entry_type=?5) AND (?6 IS NULL OR e.custom_type=?6) AND (?8 IS NULL OR {cursor_filter}) ORDER BY b.entry_seq {order} LIMIT ?7"))?;
        let ids = statement
            .query_map(
                params![
                    session,
                    branch,
                    lower,
                    upper,
                    entry_type,
                    query.custom_type,
                    remaining,
                    cursor
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        output.extend(ids);
        if stop.is_some() {
            break;
        }
    }
    Ok(output)
}

pub fn append_to_branch_index(
    db: &Connection,
    session: &str,
    entry: &Entry,
) -> Result<(), ValueError> {
    let kind = serde_json::to_value(entry.entry_type())?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let existing = if let Some(parent) = &entry.parent_id {
        // PERF-DIVERGENCE(senpi): upstream re-parses this lookup per write; we use the prepared-statement cache. Same SQL, same row.
        db.prepare_cached("SELECT branch_id FROM branch_meta WHERE session_id=?1 AND tip_entry_id=?2")?
            .query_row(params![session, parent], |row| row.get::<_, String>(0))
            .optional()?
    } else {
        None
    };
    if let Some(branch) = existing {
        // PERF-DIVERGENCE(senpi): upstream re-parses these statements per write; we use the connection's prepared-statement cache. Same SQL, same rows.
        db.prepare_cached("INSERT INTO branch_entries (session_id,branch_id,entry_id,entry_seq,entry_type) VALUES (?1,?2,?3,?4,?5)")?.execute(params![session,branch,entry.id,entry.seq,kind])?;
        db.prepare_cached("UPDATE branch_meta SET tip_entry_id=?1,tip_seq=?2 WHERE session_id=?3 AND branch_id=?4")?.execute(params![entry.id,entry.seq,session,branch])?;
        return Ok(());
    }
    // PERF-DIVERGENCE(senpi): upstream decodes every entry of the session to walk the parent chain; we walk it with one indexed id/parent/seq/type lookup per hop. Same ancestor list, same error.
    let mut ancestry: Vec<(String, i64, String)> = Vec::new();
    if let Some(parent) = &entry.parent_id {
        let mut lookup = db.prepare_cached("SELECT parent_id,seq,type FROM entries WHERE session_id=?1 AND id=?2")?;
        let mut current = Some(parent.clone());
        while let Some(id) = current {
            let (next, seq, kind) = lookup
                .query_row(params![session, id], |row| {
                    Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
                })
                .optional()?
                .ok_or_else(|| ValueError::Options(format!("Branch cache missing entry {id}")))?;
            ancestry.push((id, seq, kind));
            current = next;
        }
    }
    // The `type` column holds the serde tag of `EntryKind`, so "compaction" is `EntryType::Compaction`.
    let compaction = ancestry.iter().find(|e| e.2 == "compaction");
    let base = if let Some(compaction) = compaction {
        Some(db.query_row("SELECT b.branch_id,b.entry_seq FROM branch_entries b JOIN branch_meta m ON m.session_id=b.session_id AND m.branch_id=b.branch_id WHERE b.session_id=?1 AND b.entry_id=?2 AND ((m.base_seq IS NULL AND b.entry_seq>0) OR (m.base_seq IS NOT NULL AND b.entry_seq>m.base_seq)) AND b.entry_seq<=m.tip_seq ORDER BY m.tip_seq DESC,b.branch_id LIMIT 1",params![session,compaction.0],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?)
    } else {
        None
    };
    db.prepare_cached("INSERT INTO branch_meta (session_id,branch_id,tip_entry_id,tip_seq,base_branch_id,base_seq) VALUES (?1,?2,?2,?3,?4,?5)")?.execute(params![session,entry.id,entry.seq,base.as_ref().map(|b|&b.0),base.as_ref().map(|b|b.1)])?;
    for ancestor in ancestry
        .into_iter()
        .rev()
        .filter(|e| base.as_ref().is_none_or(|b| e.1 > b.1))
    {
        let kind = ancestor.2;
        db.prepare_cached("INSERT INTO branch_entries (session_id,branch_id,entry_id,entry_seq,entry_type) VALUES (?1,?2,?3,?4,?5)")?.execute(params![session,entry.id,ancestor.0,ancestor.1,kind])?;
    }
    db.prepare_cached("INSERT INTO branch_entries (session_id,branch_id,entry_id,entry_seq,entry_type) VALUES (?1,?2,?3,?4,?5)")?.execute(params![session,entry.id,entry.id,entry.seq,kind])?;
    Ok(())
}
