//! Retention of consumed completion records (`reflection/completions/<runId>.json`, design 2.2).

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::reflection::run_id_high_water::{read_reflection_run_id_mark, reflection_run_number};
use crate::support::time::parse_rfc3339;

use super::budget::{PassBudget, RetentionStore};
use super::guard::LazyRetentionGuard;
use super::policy::{
    RetentionContext, RetentionKeepReason, RetentionStoreResult, StoreTally, cutoff_for_days,
    list_names,
};

/// `read_reflection_health` reads the latest records for its streak and last-success window.
pub const COMPLETIONS_KEEP_NEWEST: usize = 50;

#[derive(Debug, Clone)]
pub struct CompletionRecord {
    pub run_id: String,
    pub finished_at: i64,
    pub consumed: bool,
}

pub fn read_completion(path: &Path, run_id: &str) -> Option<CompletionRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    let obj = val.as_object()?;
    if obj.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    if obj.get("runId").and_then(Value::as_str) != Some(run_id) {
        return None;
    }
    let finished_at = obj
        .get("finishedAt")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)?;
    let status = obj
        .get("delivery")
        .and_then(|d| d.get("status"))
        .and_then(Value::as_str)?;
    if status != "pending" && status != "consumed" {
        return None;
    }
    Some(CompletionRecord {
        run_id: run_id.to_owned(),
        finished_at,
        consumed: status == "consumed",
    })
}

fn keep_by_guard(
    context: &RetentionContext<'_>,
    lazy_guard: &mut LazyRetentionGuard<'_>,
    number: u64,
) -> Result<Option<RetentionKeepReason>, String> {
    let Some(guard) = lazy_guard.get()? else {
        return Ok(Some(RetentionKeepReason::SchedulerBusy));
    };
    let mark = read_reflection_run_id_mark(&context.paths.reflection);
    let Some(mark) = mark else {
        return Ok(Some(RetentionKeepReason::NoMark));
    };
    if number == 0 || number > mark {
        return Ok(Some(RetentionKeepReason::NoMark));
    }
    if number == guard.high_water {
        return Ok(Some(RetentionKeepReason::Anchor));
    }
    Ok(None)
}

pub fn prune_completions(
    context: &RetentionContext<'_>,
    lazy_guard: &mut LazyRetentionGuard<'_>,
    budget: &mut PassBudget<'_>,
) -> Result<RetentionStoreResult, String> {
    let dir = context.paths.reflection.join("completions");
    let mut tally = StoreTally::default();
    let mut records = Vec::new();

    for name in list_names(&dir) {
        let Some(run_id) = name.strip_suffix(".json") else {
            continue;
        };
        if !budget.proceed(RetentionStore::Completions) {
            return Ok(tally.result());
        }
        match read_completion(&dir.join(&name), run_id) {
            None => tally.keep(run_id, RetentionKeepReason::Malformed),
            Some(record) => records.push(record),
        }
    }

    let mut newest_first = records.clone();
    newest_first.sort_by(|l, r| {
        r.finished_at
            .cmp(&l.finished_at)
            .then_with(|| r.run_id.cmp(&l.run_id))
    });
    let newest: BTreeSet<String> = newest_first
        .into_iter()
        .take(COMPLETIONS_KEEP_NEWEST)
        .map(|r| r.run_id)
        .collect();

    records.sort_by(|l, r| l.run_id.cmp(&r.run_id));
    let cutoff = cutoff_for_days(context.now(), context.policy.completions_days);

    for record in records {
        if budget.skip(RetentionStore::Completions, &record.run_id) {
            continue;
        }
        if !budget.admit(RetentionStore::Completions) {
            break;
        }
        let number = reflection_run_number(&record.run_id);
        if newest.contains(&record.run_id) {
            tally.keep(record.run_id.clone(), RetentionKeepReason::Newest);
        } else if !record.consumed {
            tally.keep(record.run_id.clone(), RetentionKeepReason::PendingDelivery);
        } else if record.finished_at > cutoff {
            tally.keep(record.run_id.clone(), RetentionKeepReason::Recent);
        } else if context
            .paths
            .reflection
            .join("runs")
            .join(&record.run_id)
            .exists()
        {
            tally.keep(record.run_id.clone(), RetentionKeepReason::RunPresent);
        } else if let Some(reason) = keep_by_guard(context, lazy_guard, number)? {
            tally.keep(record.run_id.clone(), reason);
        } else {
            if let Err(err) = std::fs::remove_file(dir.join(format!("{}.json", record.run_id)))
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err.to_string());
            }
            tally.removed.push(record.run_id.clone());
        }
        budget.done(RetentionStore::Completions, &record.run_id);
    }

    Ok(tally.result())
}
