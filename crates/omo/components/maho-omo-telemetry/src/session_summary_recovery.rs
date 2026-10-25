//! Lazy recovery sweeps and bounded durable resends.
use crate::{
    session_summary_storage::*, session_summary_store::SummaryStore,
    session_summary_window::summary_properties,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io, path::Path, sync::Arc};

pub type SummaryPublish = Arc<dyn Fn(Value) -> bool + Send + Sync>;
pub const PENDING_CAP: usize = 200;
pub const PENDING_CAP_BYTES: u64 = 1024 * 1024;
pub const RESENDS_PER_SWEEP: usize = 20;
#[derive(Default)]
pub struct SummaryRecovery {
    backoff: BTreeMap<String, (f64, f64)>,
    pub dropped: u64,
}
impl SummaryRecovery {
    pub fn deliver(
        &self,
        store: &SummaryStore,
        record: &Value,
        consent: &dyn Fn(&Path) -> bool,
        publish: &SummaryPublish,
    ) -> io::Result<()> {
        let uuid = record["uuid"]
            .as_str()
            .ok_or_else(|| io::Error::other("pending uuid missing"))?;
        if !is_record_sendable(record, consent) || publish(record.clone()) {
            store.remove_pending(uuid)?;
        }
        Ok(())
    }
    pub fn sweep(
        &mut self,
        store: &SummaryStore,
        now: f64,
        consent: &dyn Fn(&Path) -> bool,
        publish: &SummaryPublish,
    ) -> io::Result<()> {
        store.sweep_leftovers()?;
        let uuids = store.pending_uuids()?;
        let pending_at_start: std::collections::BTreeSet<_> = uuids.iter().cloned().collect();
        let mut aged = Vec::new();
        for uuid in uuids {
            let record = store.read_pending(&uuid)?.unwrap_or(Value::Null);
            let bytes = match std::fs::metadata(store.pending_path(&uuid)) {
                Ok(m) => m.len(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e),
            };
            aged.push((uuid, record, bytes));
        }
        aged.sort_by(|a, b| {
            a.1["createdAt"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b.1["createdAt"].as_f64().unwrap_or(0.0))
        });
        let mut files = aged.len();
        let mut bytes: u64 = aged.iter().map(|r| r.2).sum();
        let mut resends = 0;
        for (uuid, mut record, size) in aged {
            let sendable = is_record_sendable(&record, consent);
            if files > PENDING_CAP
                || bytes > PENDING_CAP_BYTES
                || now - record["createdAt"].as_f64().unwrap_or(now) > SUMMARY_TTL_MS
            {
                store.remove_pending(&uuid)?;
                self.backoff.remove(&uuid);
                self.dropped += u64::from(sendable);
                files -= 1;
                bytes -= size;
                continue;
            }
            if !sendable {
                store.remove_pending(&uuid)?;
                continue;
            }
            if record["owner"] != store.owner && store.is_owner_alive(&record["owner"])? {
                continue;
            }
            if resends >= RESENDS_PER_SWEEP
                || self.backoff.get(&uuid).is_some_and(|(at, _)| now < *at)
            {
                continue;
            }
            resends += 1;
            let delay = (self
                .backoff
                .get(&uuid)
                .map_or(30_000.0, |(_, delay)| *delay)
                * 2.0)
                .min(3_600_000.0);
            self.backoff.insert(uuid.clone(), (now + delay, delay));
            if record["owner"] != store.owner {
                let Some(claim) = store.claim_pending(&uuid)? else {
                    continue;
                };
                let Some(owned) = claim.record else {
                    remove_file(&claim.claim_path)?;
                    continue;
                };
                record = owned;
                record["owner"] = store.owner.clone();
                store.write_pending(&record)?;
                remove_file(&claim.claim_path)?;
            }
            self.deliver(store, &record, consent, publish)?;
        }
        for key in store.window_keys()? {
            let Some(record) = store.read(&key)? else {
                if let Some(claim) = store.claim_window(&key)? {
                    remove_file(&claim.claim_path)?;
                    maho_ext_api::diagnostic("unreadable session summary window was removed");
                }
                continue;
            };
            if !is_record_sendable(&record, consent) {
                store.remove(&key)?;
                continue;
            }
            let age = now - record["updatedAt"].as_f64().unwrap_or(now);
            if age > SUMMARY_TTL_MS {
                self.dropped += u64::from(!record["window"].is_null());
                store.remove(&key)?;
                continue;
            }
            if record["window"].is_null()
                || age <= OWNER_STALE_MS
                || store.is_owner_alive(&record["owner"])?
            {
                continue;
            }
            let Some(claim) = store.claim_window(&key)? else {
                continue;
            };
            let Some(mut record) = claim.record else {
                remove_file(&claim.claim_path)?;
                continue;
            };
            if !is_record_sendable(&record, consent) {
                remove_file(&claim.claim_path)?;
                continue;
            }
            let seq = record["nextSeq"].as_u64().unwrap_or(0);
            let id = record["windowId"]
                .as_str()
                .filter(|id| safe_uuid(id))
                .map(str::to_owned)
                .map(Ok)
                .unwrap_or_else(new_summary_id)?;
            let epoch = new_summary_epoch()?;
            let pending = if pending_at_start.contains(&id) || store.has_pending(&id)? {
                None
            } else {
                let mut properties = summary_properties(
                    record["sessionHash"].as_str().unwrap_or_default(),
                    &record["window"],
                    seq,
                    "recovered",
                    record["sessionRole"].as_str().unwrap_or("main"),
                    record["sampleRate"].as_f64().unwrap_or(0.05),
                );
                properties["summary_epoch"] = json!(epoch);
                properties["dropped_events"] = json!(std::mem::take(&mut self.dropped));
                let pending = json!({"v":2,"consent":record["consent"],"scope":record["scope"],"uuid":id,"sessionHash":record["sessionHash"],"owner":store.owner,"createdAt":now,"properties":properties});
                store.write_pending(&pending)?;
                Some(pending)
            };
            record["nextSeq"] = json!(seq + 1);
            record["window"] = Value::Null;
            record["windowId"] = Value::Null;
            record["owner"] = store.owner.clone();
            record["epoch"] = json!(epoch);
            record["updatedAt"] = json!(now);
            if !key.contains('@') {
                store.write_window(&key, &record, true)?;
            }
            remove_file(&claim.claim_path)?;
            if let Some(pending) = pending {
                self.deliver(store, &pending, consent, publish)?;
            }
        }
        Ok(())
    }
}
