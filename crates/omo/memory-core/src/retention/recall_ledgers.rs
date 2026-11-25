//! Per-session recall ledger retention (`recall/ledgers/<session>.json`, design 2.5).

use std::time::UNIX_EPOCH;

use super::budget::{PassBudget, RetentionStore};
use super::policy::{
    RetentionContext, RetentionKeepReason, RetentionStoreResult, StoreTally, cutoff_for_days,
    list_names,
};

pub fn prune_recall_ledgers(
    context: &RetentionContext<'_>,
    budget: &mut PassBudget<'_>,
) -> std::io::Result<RetentionStoreResult> {
    let mut tally = StoreTally::default();
    let cutoff = cutoff_for_days(context.now(), context.policy.completions_days);

    for name in list_names(&context.paths.recall_ledger) {
        let Some(session_id) = name.strip_suffix(".json") else {
            continue;
        };
        if budget.skip(RetentionStore::Ledgers, &name) {
            continue;
        }
        if !budget.admit(RetentionStore::Ledgers) {
            break;
        }
        let path = context.paths.recall_ledger.join(&name);
        if let Ok(meta) = std::fs::symlink_metadata(&path)
            && meta.is_file()
        {
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or(i64::MAX);
            if context.is_live_session.is_some_and(|live| live(session_id)) {
                tally.keep(session_id, RetentionKeepReason::Live);
            } else if mtime_ms > cutoff {
                tally.keep(session_id, RetentionKeepReason::Recent);
            } else {
                match std::fs::remove_file(&path) {
                    Ok(()) => tally.removed.push(session_id.to_owned()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
            }
        }
        budget.done(RetentionStore::Ledgers, &name);
    }
    Ok(tally.result())
}
