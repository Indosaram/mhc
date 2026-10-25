use crate::session_summary_storage::*;
use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
};

pub struct SummaryStore {
    pub(crate) dir: PathBuf,
    pub(crate) owner: Value,
}
impl SummaryStore {
    pub fn new(state: &Path) -> Self {
        Self {
            dir: state.join("rollups"),
            owner: current_process_owner(),
        }
    }
    pub(crate) fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json"))
    }
    pub(crate) fn pending_path(&self, uuid: &str) -> PathBuf {
        self.dir.join("pending").join(format!("{uuid}.json"))
    }
    pub(crate) fn register(&self, now: f64) -> io::Result<()> {
        let pid = self.owner["pid"].as_i64().unwrap_or_default();
        let path = self.dir.join("owners").join(format!("{pid}.json"));
        if let Some(entry) = read_json(&path)? {
            if entry["bootId"] == self.owner["bootId"]
                && now - entry["heartbeatAt"].as_f64().unwrap_or(0.0) < 60_000.0
            {
                return Ok(());
            }
        }
        write_json_atomic(
            &path,
            &json!({"pid":pid,"bootId":self.owner["bootId"],"startTime":read_process_start_time(pid),"heartbeatAt":now}),
            &self.owner,
            false,
        )?;
        Ok(())
    }
    pub fn read(&self, hash: &str) -> io::Result<Option<Value>> {
        if !safe_name(hash) {
            return Err(io::Error::other("invalid summary key"));
        }
        Ok(read_json(&self.path(hash))?.and_then(parse_window_record))
    }
    pub fn write(&self, rollup: &Value) -> io::Result<()> {
        let hash = rollup["storageKey"]
            .as_str()
            .or_else(|| rollup["sessionHash"].as_str())
            .filter(|s| safe_name(s))
            .ok_or_else(|| io::Error::other("invalid rollup identity"))?;
        self.write_window(hash, rollup, false).map(|_| ())
    }
    pub(crate) fn write_window(
        &self,
        key: &str,
        value: &Value,
        exclusive: bool,
    ) -> io::Result<bool> {
        if !safe_name(key) {
            return Err(io::Error::other("invalid summary key"));
        }
        let mut record = value.clone();
        record["v"] = json!(2);
        record["seq"] = record["nextSeq"].clone();
        record["heartbeatAt"] = record["updatedAt"].clone();
        self.register(record["updatedAt"].as_f64().unwrap_or(0.0))?;
        write_json_atomic(&self.path(key), &record, &self.owner, exclusive)
    }
    pub fn remove(&self, hash: &str) -> io::Result<()> {
        if !safe_name(hash) {
            return Err(io::Error::other("invalid summary key"));
        }
        remove_file(&self.path(hash))
    }
    pub(crate) fn window_keys(&self) -> io::Result<Vec<String>> {
        Ok(list_names(&self.dir)?
            .into_iter()
            .filter_map(|name| {
                name.strip_suffix(".json")
                    .filter(|key| safe_name(key))
                    .map(str::to_owned)
            })
            .collect())
    }
    pub(crate) fn write_pending(&self, record: &Value) -> io::Result<()> {
        let uuid = record["uuid"]
            .as_str()
            .filter(|id| safe_uuid(id))
            .ok_or_else(|| io::Error::other("invalid pending uuid"))?;
        self.register(record["createdAt"].as_f64().unwrap_or(0.0))?;
        write_json_atomic(&self.pending_path(uuid), record, &self.owner, false)?;
        Ok(())
    }
    pub(crate) fn pending_uuids(&self) -> io::Result<Vec<String>> {
        Ok(list_names(&self.dir.join("pending"))?
            .into_iter()
            .filter_map(|name| {
                name.strip_suffix(".json")
                    .filter(|id| safe_uuid(id))
                    .map(str::to_owned)
            })
            .collect())
    }
    pub(crate) fn read_pending(&self, uuid: &str) -> io::Result<Option<Value>> {
        Ok(read_json(&self.pending_path(uuid))?
            .map(|v| parse_pending_record(v).unwrap_or(Value::Null)))
    }
    pub(crate) fn remove_pending(&self, uuid: &str) -> io::Result<()> {
        remove_file(&self.pending_path(uuid))
    }
    /// Kept for callers of the legacy store API; claims remain durable until explicitly completed.
    pub fn claim_orphans(&self, now: f64) -> io::Result<Vec<Value>> {
        let mut records = Vec::new();
        for key in self.window_keys()? {
            let Some(record) = self.read(&key)? else {
                continue;
            };
            if now - record["updatedAt"].as_f64().unwrap_or(now) <= OWNER_STALE_MS
                || self.is_owner_alive(&record["owner"])?
            {
                continue;
            }
            if let Some(claim) = self.claim_window(&key)? {
                if let Some(record) = claim.record {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }
}
pub(crate) fn parse_window_record(mut value: Value) -> Option<Value> {
    if value["v"] == 2 {
        if !valid_owner(&value["owner"])
            || value["sampleRate"].as_f64().is_none()
            || value["seq"].as_u64().is_none()
            || value["heartbeatAt"].as_f64().is_none()
        {
            return None;
        }
        value["nextSeq"] = value["seq"].clone();
        value["updatedAt"] = value["heartbeatAt"].clone();
        value["pid"] = value["owner"]["pid"].clone();
        if !value["window"].is_null() && !value["windowId"].as_str().is_some_and(safe_uuid) {
            return None;
        }
    } else if value["v"] == 1 {
        value["owner"] = json!({"pid":value["pid"],"bootId":"legacy"});
    } else {
        return None;
    }
    if value["pid"].as_i64().is_none()
        || !value["sessionHash"].is_string()
        || value["nextSeq"].as_u64().is_none()
        || value["updatedAt"].as_f64().is_none()
    {
        return None;
    }
    if !matches!(
        value["sessionRole"].as_str(),
        Some("main" | "child" | "dag_child" | "member")
    ) {
        value["sessionRole"] = json!("main");
    }
    if !value["window"].is_null() {
        value["window"] = crate::session_summary_window::parse_window(&value["window"])?;
    }
    Some(value)
}
pub fn rollup(hash: &str, seq: u64, window: Value, cwd: &Path, now: f64, role: &str) -> Value {
    json!({"v":2,"pid":std::process::id(),"owner":current_process_owner(),"sessionHash":hash,"sessionRole":role,"seq":seq,"nextSeq":seq,"window":window,"windowId":null,"epoch":null,"sampleRate":0.05,"updatedAt":now,"heartbeatAt":now,"consent":"enabled","scope":{"cwd":cwd}})
}
