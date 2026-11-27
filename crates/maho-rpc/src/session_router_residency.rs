//! Port of senpi `packages/coding-agent/src/modes/rpc/session-router-residency.ts`.
//!
//! Manages retained session handover park records (resident stamps, live markers, transcript
//! switch following, and cleanup on explicit close/reclaim).

use crate::host_handover_parked::{
    HandoverParkedEntry, clear_handover_parked_session, record_handover_parked_session,
};
use std::path::Path;

pub trait ResidencyEntryView {
    fn session_id(&self) -> &str;
    fn durable_session_id(&self) -> Option<&str>;
    fn session_path(&self) -> Option<&str>;
    fn recorded_path(&self) -> Option<&str>;
    fn set_recorded_path(&mut self, path: Option<String>);
    fn retain_on_disconnect(&self) -> bool;
    fn is_open(&self) -> bool;
    fn build_parked_record(
        &self,
        session_path: &str,
        resident: bool,
        live: bool,
    ) -> Option<HandoverParkedEntry>;
}

pub async fn sync_retained_record(
    handover_parked_dir: Option<&Path>,
    entry: &mut dyn ResidencyEntryView,
    requested_path: Option<&str>,
) {
    let Some(dir) = handover_parked_dir else {
        return;
    };
    let session_path = entry.session_path().map(str::to_owned);
    if entry.retain_on_disconnect()
        && let Some(ref path) = session_path
    {
        let previous = entry.recorded_path().map(str::to_owned);
        entry.set_recorded_path(Some(path.clone()));
        if let Some(prev) = previous
            && prev != *path
        {
            let _ = clear_handover_parked_session(dir, &prev).await;
        }
        if let Some(record) = entry.build_parked_record(path, true, true)
            && let Err(err) = record_handover_parked_session(dir, &record).await
        {
            eprintln!(
                "senpi rpc retained session record {} failed: {err}",
                entry.session_id()
            );
        }
        if requested_path.is_none_or(|req| req == path) {
            return;
        }
    }
    if let Some(req) = requested_path {
        let _ = clear_handover_parked_session(dir, req).await;
    }
}

pub async fn clear_retained_record(
    handover_parked_dir: Option<&Path>,
    entry: &mut dyn ResidencyEntryView,
) {
    let Some(dir) = handover_parked_dir else {
        return;
    };
    let mut keys = Vec::new();
    if let Some(p) = entry.recorded_path() {
        keys.push(p.to_owned());
    }
    if let Some(p) = entry.session_path()
        && !keys.iter().any(|k| k == p)
    {
        keys.push(p.to_owned());
    }
    entry.set_recorded_path(None);
    for key in keys {
        let _ = clear_handover_parked_session(dir, &key).await;
    }
}
