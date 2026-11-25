//! Port of senpi packages/coding-agent/src/core/session-import/jsonl-import.ts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use maho_agent::durable::store::{DurableStore, StoreValue, StoreWrite};
use maho_agent::durable::store_admission::AdmissionLine;
use maho_agent::durable::{
    SessionHeader, SessionState, append_record_writes, divergence_write, manifest_write,
    read_session_state, rewrite_record_write,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::paths::{PathInputOptions, normalize_path};
use crate::session_manager::migrate_session_entries;

const PREFIX_DOMAIN: &str = "senpi-jsonl-prefix/1";
const LEDGER_PREFIX: &str = "session-import/file/";

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlFileStamp {
    pub size: u64,
    pub mtime_ms: f64,
    pub ctime_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HatchMarker {
    pub size: u64,
    pub mtime_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighWaterMark {
    pub id: String,
    pub hash: String,
    pub offset: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriterKind {
    Store,
    Hatch,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlImportLedger {
    pub size: u64,
    pub mtime_ms: f64,
    pub ctime_ms: f64,
    pub path: String,
    pub source_session_id: String,
    pub session_id: String,
    pub high_water_mark: HighWaterMark,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_digest: Option<String>,
    pub writer_kind: WriterKind,
    pub checked_at: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Divergence {
    pub kind: String,
    pub parent_session_id: String,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonlImportOutcome {
    Imported,
    Extended,
    Forked,
    Adopted,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonlImportResult {
    pub outcome: JsonlImportOutcome,
    pub session_id: String,
    pub imported: usize,
}

#[derive(Clone, Debug)]
pub struct ImportedSessionView {
    pub id: String,
    pub header: SessionHeader,
    pub entries: Vec<StoreValue>,
    pub divergence: Option<Divergence>,
}

#[derive(Clone, Debug)]
pub struct ProjectedTail {
    pub key: String,
    pub line: String,
    pub offset: u64,
}

#[async_trait::async_trait]
pub trait JsonlFiles: Send + Sync {
    async fn stat(&self, path: &Path) -> std::io::Result<JsonlFileStamp>;
    async fn read_range(&self, path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>>;
    async fn marker(&self, path: &Path) -> std::io::Result<Option<HatchMarker>>;
}

pub struct RealJsonlFiles;

fn system_time_ms(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

#[cfg(unix)]
fn ctime_ms_of(meta: &std::fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    (meta.ctime() as f64) * 1000.0 + (meta.ctime_nsec() as f64) / 1_000_000.0
}

#[cfg(not(unix))]
fn ctime_ms_of(meta: &std::fs::Metadata) -> f64 {
    meta.modified().map_or(0.0, system_time_ms)
}

pub fn stamp_from_metadata(meta: &std::fs::Metadata) -> JsonlFileStamp {
    JsonlFileStamp {
        size: meta.len(),
        mtime_ms: meta.modified().map_or(0.0, system_time_ms),
        ctime_ms: ctime_ms_of(meta),
    }
}

#[async_trait::async_trait]
impl JsonlFiles for RealJsonlFiles {
    async fn stat(&self, path: &Path) -> std::io::Result<JsonlFileStamp> {
        let meta = tokio::fs::metadata(path).await?;
        Ok(stamp_from_metadata(&meta))
    }

    async fn read_range(&self, path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        if end <= start {
            return Ok(Vec::new());
        }
        let len = usize::try_from(end - start).unwrap_or(0);
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buf = vec![0u8; len];
        let mut read = 0usize;
        while read < len {
            let n = file.read(&mut buf[read..]).await?;
            if n == 0 {
                break;
            }
            read += n;
        }
        buf.truncate(read);
        Ok(buf)
    }

    async fn marker(&self, path: &Path) -> std::io::Result<Option<HatchMarker>> {
        let marker_path = PathBuf::from(format!("{}.k9f2", path.display()));
        match tokio::fs::read_to_string(&marker_path).await {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(val) => {
                    let size = val.get("size").and_then(Value::as_u64);
                    let mtime_ms = val.get("mtimeMs").and_then(Value::as_f64);
                    if let (Some(size), Some(mtime_ms)) = (size, mtime_ms) {
                        Ok(Some(HatchMarker { size, mtime_ms }))
                    } else {
                        Ok(None)
                    }
                }
                Err(_) => Ok(None),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Writes the `.k9f2` hatch marker atomically.
pub async fn write_hatch_marker(path: &Path, written: HatchMarker) -> std::io::Result<()> {
    let marker_path = PathBuf::from(format!("{}.k9f2", path.display()));
    let temp = PathBuf::from(format!(
        "{}.{}",
        marker_path.display(),
        uuid::Uuid::new_v4()
    ));
    let payload = serde_json::to_vec(&written).map_err(std::io::Error::other)?;
    let write_res: std::io::Result<()> = async {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&temp).await?;
        file.write_all(&payload).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, &marker_path).await
    }
    .await;
    if let Err(err) = write_res {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(err);
    }
    Ok(())
}

fn chain_line(previous: &str, raw: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(previous.as_bytes());
    hash.update(b"\0");
    hash.update(raw.as_bytes());
    hex::encode(hash.finalize())
}

fn chain_of<'a>(raws: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut digests = Vec::new();
    let mut digest = hex::encode(Sha256::digest(PREFIX_DOMAIN.as_bytes()));
    for raw in raws {
        digest = chain_line(&digest, raw);
        digests.push(digest.clone());
    }
    digests
}

fn hash_short(bytes: &[u8]) -> String {
    let full = hex::encode(Sha256::digest(bytes));
    full[..32].to_owned()
}

fn same_stamp(a: JsonlFileStamp, b: JsonlFileStamp) -> bool {
    a.size == b.size && a.mtime_ms == b.mtime_ms && a.ctime_ms == b.ctime_ms
}

fn ledger_key(path: &str) -> String {
    format!("{LEDGER_PREFIX}{path}")
}

fn divergence_of(value: &StoreValue) -> Option<Divergence> {
    let obj = value.as_object()?;
    if obj.get("kind").and_then(Value::as_str) != Some("k9f2") {
        return None;
    }
    let parent_session_id = obj
        .get("parentSessionId")
        .and_then(Value::as_str)?
        .to_owned();
    let note = obj.get("note").and_then(Value::as_str)?.to_owned();
    Some(Divergence {
        kind: "k9f2".into(),
        parent_session_id,
        note,
    })
}

fn is_truthy_json(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

struct LocatedRecord {
    value: StoreValue,
    key: String,
    stored: String,
    raw: String,
    offset: u64,
    end: u64,
}

fn migrate_v1_positional(values: &mut [StoreValue]) {
    let mut previous: Option<String> = None;
    let mut ordinal = 0usize;
    let mut assigned_ids: Vec<Option<String>> = vec![None; values.len()];
    for (idx, value) in values.iter_mut().enumerate() {
        let Some(entry) = value.as_object_mut() else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) == Some("session") {
            entry.insert("version".into(), json!(2));
            continue;
        }
        let id = format!("legacy-{ordinal}");
        ordinal += 1;
        entry.insert("id".into(), Value::String(id.clone()));
        entry.insert(
            "parentId".into(),
            previous
                .as_ref()
                .map_or(Value::Null, |p| Value::String(p.clone())),
        );
        previous = Some(id.clone());
        assigned_ids[idx] = Some(id);
    }
    for value in values.iter_mut() {
        let Some(entry) = value.as_object_mut() else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) == Some("compaction")
            && let Some(idx) = entry
                .get("firstKeptEntryIndex")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
        {
            if let Some(Some(target_id)) = assigned_ids.get(idx) {
                entry.insert("firstKeptEntryId".into(), Value::String(target_id.clone()));
            }
            entry.remove("firstKeptEntryIndex");
        }
    }
}

struct ParsedJsonl {
    header: LocatedRecord,
    entries: Vec<LocatedRecord>,
}

fn parse_jsonl(bytes: &[u8]) -> Result<ParsedJsonl, String> {
    struct RawLine {
        value: StoreValue,
        raw: String,
        offset: u64,
        end: u64,
    }
    let mut raw_lines = Vec::new();
    let mut offset = 0usize;
    for end in 0..=bytes.len() {
        if end != bytes.len() && bytes[end] != b'\n' {
            continue;
        }
        let raw = String::from_utf8_lossy(&bytes[offset..end]).into_owned();
        if let Ok(value) = serde_json::from_str::<StoreValue>(&raw)
            && is_truthy_json(&value)
        {
            raw_lines.push(RawLine {
                value,
                raw,
                offset: u64::try_from(offset).unwrap_or(u64::MAX),
                end: u64::try_from(end).unwrap_or(u64::MAX),
            });
        }
        offset = end + 1;
    }

    let mut values: Vec<StoreValue> = raw_lines.iter().map(|l| l.value.clone()).collect();
    let Some(first) = values.first() else {
        return Err("JSONL session has no header".to_owned());
    };
    let Some(first_obj) = first.as_object() else {
        return Err("JSONL session has no header".to_owned());
    };
    if first_obj.get("type").and_then(Value::as_str) != Some("session")
        || first_obj.get("id").and_then(Value::as_str).is_none()
    {
        return Err("JSONL session has no header".to_owned());
    }
    let version = first_obj
        .get("version")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    if version < 2 {
        migrate_v1_positional(&mut values);
    }
    if version < 3 {
        migrate_session_entries(&mut values);
    }

    let mut iter = raw_lines.into_iter().zip(values);
    let (header_line, header_val) = iter
        .next()
        .ok_or_else(|| "JSONL session has no header".to_owned())?;
    let header_id = header_val
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "JSONL session has no header".to_owned())?
        .to_owned();
    let header_stored = if version < 3 {
        serde_json::to_string(&header_val).map_err(|e| e.to_string())?
    } else {
        header_line.raw.clone()
    };
    let header = LocatedRecord {
        value: header_val,
        key: header_id,
        stored: header_stored,
        raw: header_line.raw,
        offset: header_line.offset,
        end: header_line.end,
    };

    let mut keys = HashSet::new();
    let mut entries = Vec::new();
    for (index, (line, val)) in iter.enumerate() {
        if val.get("type").and_then(Value::as_str) == Some("session") {
            return Err("Duplicate JSONL session entry".to_owned());
        }
        let id = val.get("id").and_then(Value::as_str);
        let key = match id {
            None => format!("~line-{index}"),
            Some(id) if id.starts_with('~') => format!("~{id}"),
            Some(id) => id.to_owned(),
        };
        if !keys.insert(key.clone()) {
            return Err("Duplicate JSONL session entry".to_owned());
        }
        let stored = if version < 3 {
            serde_json::to_string(&val).map_err(|e| e.to_string())?
        } else {
            line.raw.clone()
        };
        entries.push(LocatedRecord {
            value: val,
            key,
            stored,
            raw: line.raw,
            offset: line.offset,
            end: line.end,
        });
    }

    Ok(ParsedJsonl { header, entries })
}

fn high_water_mark(record: &LocatedRecord) -> HighWaterMark {
    HighWaterMark {
        id: record.key.clone(),
        hash: hash_short(record.raw.as_bytes()),
        offset: record.offset,
        end: record.end,
    }
}

pub struct JsonlImporterOptions {
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
    pub files: Option<Arc<dyn JsonlFiles>>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub mtime_tick_ms: Option<f64>,
}

pub struct JsonlImporter {
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
    files: Arc<dyn JsonlFiles>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    tick: f64,
}

impl JsonlImporter {
    pub fn new(options: JsonlImporterOptions) -> Self {
        let now = options.now.unwrap_or_else(|| {
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
            })
        });
        Self {
            store: options.store,
            epoch: options.epoch,
            admission: options.admission,
            files: options.files.unwrap_or_else(|| Arc::new(RealJsonlFiles)),
            now,
            tick: options.mtime_tick_ms.unwrap_or(1.0),
        }
    }

    async fn manifest(&self, id: &str) -> Result<Option<SessionState>, String> {
        read_session_state(self.store.as_ref(), id)
            .await
            .map_err(|e| e.to_string())
    }

    async fn ledger(&self, path: &str) -> Result<Option<JsonlImportLedger>, String> {
        let val = self
            .store
            .get_document(&ledger_key(path))
            .await
            .map_err(|e| e.to_string())?;
        Ok(val.and_then(|v| serde_json::from_value(v).ok()))
    }

    async fn commit(&self, writes: Vec<StoreWrite>) -> Result<(), String> {
        self.store
            .commit(self.epoch, writes, Box::new(|_| {}))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn resolve_path(path: &Path) -> String {
        normalize_path(&path.to_string_lossy(), &PathInputOptions::default())
    }

    async fn import_file_inner(&self, path_str: &str) -> Result<JsonlImportResult, String> {
        let path = Path::new(path_str);
        let stamp = self.files.stat(path).await.map_err(|e| e.to_string())?;
        let previous = self.ledger(path_str).await?;
        if let Some(prev) = &previous
            && stamp.size == prev.size
        {
            let uncertain = (prev.checked_at - prev.mtime_ms).abs() <= self.tick;
            let same_mtime = stamp.mtime_ms == prev.mtime_ms;
            let same_high_water = if !same_mtime || uncertain {
                let hwm = &prev.high_water_mark;
                let bytes = self
                    .files
                    .read_range(path, hwm.offset, hwm.end)
                    .await
                    .map_err(|e| e.to_string())?;
                hash_short(&bytes) == hwm.hash
            } else {
                true
            };
            if same_mtime && same_high_water && stamp.ctime_ms == prev.ctime_ms {
                if uncertain {
                    let mut updated = prev.clone();
                    updated.checked_at = (self.now)();
                    self.commit(vec![StoreWrite::Put {
                        key: ledger_key(path_str),
                        data: serde_json::to_value(updated).map_err(|e| e.to_string())?,
                    }])
                    .await?;
                }
                return Ok(JsonlImportResult {
                    outcome: JsonlImportOutcome::Unchanged,
                    session_id: prev.session_id.clone(),
                    imported: 0,
                });
            }
        }

        let bytes = self
            .files
            .read_range(path, 0, stamp.size)
            .await
            .map_err(|e| e.to_string())?;
        let marker = self.files.marker(path).await.map_err(|e| e.to_string())?;
        let re_stat = self.files.stat(path).await.map_err(|e| e.to_string())?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != stamp.size
            || !same_stamp(stamp, re_stat)
        {
            return Err(
                "JSONL session changed during import; reopen it to import the next snapshot"
                    .to_owned(),
            );
        }

        let parsed = parse_jsonl(&bytes)?;
        let header = &parsed.header;
        let entries = &parsed.entries;
        let source_session_id = header.key.clone();
        let target = match &previous {
            Some(prev) if prev.source_session_id == source_session_id => prev.session_id.clone(),
            _ => source_session_id.clone(),
        };
        let held = self.manifest(&target).await?;
        let line_raws: Vec<&str> = std::iter::once(header.raw.as_str())
            .chain(entries.iter().map(|e| e.raw.as_str()))
            .collect();
        let digests = chain_of(line_raws);

        let mark_index = if let (Some(_), Some(prev)) = (&held, &previous)
            && prev.session_id == target
            && let Some(prev_digest) = &prev.prefix_digest
        {
            std::iter::once(header)
                .chain(entries.iter())
                .enumerate()
                .position(|(idx, line)| {
                    line.key == prev.high_water_mark.id && digests.get(idx) == Some(prev_digest)
                })
        } else {
            None
        };

        let writer_kind = match marker {
            Some(m) if m.size == stamp.size && m.mtime_ms == stamp.mtime_ms => WriterKind::Hatch,
            _ => WriterKind::Other,
        };

        if let (Some(held_state), Some(idx)) = (&held, mark_index) {
            let held_keys: HashSet<&str> =
                held_state.entries.iter().map(|e| e.key.as_str()).collect();
            let fresh: Vec<maho_agent::durable::StoredEntry> = entries[idx..]
                .iter()
                .filter(|record| !held_keys.contains(record.key.as_str()))
                .map(|record| maho_agent::durable::StoredEntry {
                    key: record.key.clone(),
                    line: record.stored.clone(),
                })
                .collect();
            let tail_rec = entries.last().unwrap_or(header);
            let grown = JsonlImportLedger {
                size: stamp.size,
                mtime_ms: stamp.mtime_ms,
                ctime_ms: stamp.ctime_ms,
                path: path_str.to_owned(),
                source_session_id,
                session_id: target.clone(),
                high_water_mark: high_water_mark(tail_rec),
                prefix_digest: digests.last().cloned(),
                writer_kind,
                checked_at: (self.now)(),
            };
            let mut writes = append_record_writes(&target, held_state.rewrites, &fresh);
            writes.push(StoreWrite::Put {
                key: ledger_key(path_str),
                data: serde_json::to_value(grown).map_err(|e| e.to_string())?,
            });
            self.commit(writes).await?;
            return Ok(JsonlImportResult {
                outcome: JsonlImportOutcome::Extended,
                session_id: target,
                imported: fresh.len(),
            });
        }

        let old_entries: Vec<StoreValue> = held
            .as_ref()
            .map(|h| {
                h.entries
                    .iter()
                    .filter_map(|e| serde_json::from_str::<StoreValue>(&e.line).ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut cursor = 0usize;
        for record in entries {
            if old_entries
                .get(cursor)
                .is_some_and(|old| &record.value == old)
            {
                cursor += 1;
            }
        }
        let extends_held = cursor == old_entries.len();
        let fork = held.is_some() && !extends_held && writer_kind == WriterKind::Hatch;
        let session_id = if fork {
            self.store.mint_id()
        } else {
            target.clone()
        };
        let divergence_val = if fork {
            json!({
                "kind": "k9f2",
                "parentSessionId": target,
                "note": "Imported a developer-hatch history that diverged from its original session.",
            })
        } else {
            held.as_ref()
                .and_then(|h| divergence_of(&h.divergence))
                .map(|d| json!({"kind": d.kind, "parentSessionId": d.parent_session_id, "note": d.note}))
                .unwrap_or(Value::Null)
        };
        let mut next_header: SessionHeader = header.value.as_object().cloned().unwrap_or_default();
        next_header.insert("id".into(), Value::String(session_id.clone()));

        let next_ledger = JsonlImportLedger {
            size: stamp.size,
            mtime_ms: stamp.mtime_ms,
            ctime_ms: stamp.ctime_ms,
            path: path_str.to_owned(),
            source_session_id,
            session_id: session_id.clone(),
            high_water_mark: high_water_mark(entries.last().unwrap_or(header)),
            prefix_digest: digests.last().cloned(),
            writer_kind,
            checked_at: (self.now)(),
        };

        let old_by_key: HashMap<&str, &StoreValue> = held
            .as_ref()
            .map(|h| {
                h.entries
                    .iter()
                    .zip(old_entries.iter())
                    .map(|(e, v)| (e.key.as_str(), v))
                    .collect()
            })
            .unwrap_or_default();
        let mut imported = 0usize;
        for record in entries {
            if fork || old_by_key.get(record.key.as_str()) != Some(&&record.value) {
                imported += 1;
            }
        }
        let stored: Vec<maho_agent::durable::StoredEntry> = entries
            .iter()
            .map(|r| maho_agent::durable::StoredEntry {
                key: r.key.clone(),
                line: r.stored.clone(),
            })
            .collect();

        let is_prefix = held.as_ref().is_some_and(|h| {
            !fork
                && entries.len() >= old_entries.len()
                && old_entries
                    .iter()
                    .enumerate()
                    .all(|(i, v)| entries.get(i).is_some_and(|e| &e.value == v))
                && next_header == h.header
                && divergence_val == h.divergence
        });

        let mut writes = if is_prefix {
            let rewrites = held.as_ref().map_or(0, |h| h.rewrites);
            append_record_writes(&session_id, rewrites, &stored[old_entries.len()..])
        } else {
            vec![
                manifest_write(&session_id, &next_header),
                divergence_write(&session_id, divergence_val),
                rewrite_record_write(self.store.as_ref(), &session_id, &next_header, &stored),
            ]
        };
        writes.push(StoreWrite::Put {
            key: ledger_key(path_str),
            data: serde_json::to_value(next_ledger).map_err(|e| e.to_string())?,
        });
        self.commit(writes).await?;

        let outcome = if held.is_none() {
            JsonlImportOutcome::Imported
        } else if fork {
            JsonlImportOutcome::Forked
        } else if extends_held {
            JsonlImportOutcome::Extended
        } else {
            JsonlImportOutcome::Adopted
        };

        Ok(JsonlImportResult {
            outcome,
            session_id,
            imported,
        })
    }

    pub async fn open_paths(&self, paths: &[&Path]) -> Result<Vec<JsonlImportResult>, String> {
        let resolved: Vec<String> = paths.iter().map(|p| Self::resolve_path(p)).collect();
        self.admission
            .admit(|| async move {
                let mut results = Vec::with_capacity(resolved.len());
                for p in &resolved {
                    results.push(self.import_file_inner(p).await?);
                }
                Ok(results)
            })
            .await
    }

    pub async fn import_file(&self, path: &Path) -> Result<JsonlImportResult, String> {
        let resolved = Self::resolve_path(path);
        self.admission
            .admit(|| async move { self.import_file_inner(&resolved).await })
            .await
    }

    pub async fn get_ledger(&self, path: &Path) -> Result<Option<JsonlImportLedger>, String> {
        let resolved = Self::resolve_path(path);
        self.ledger(&resolved).await
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<ImportedSessionView>, String> {
        self.admission
            .admit(|| async move {
                let Some(held) = self.manifest(id).await? else {
                    return Ok(None);
                };
                let entries = held
                    .entries
                    .iter()
                    .map(|e| serde_json::from_str(&e.line))
                    .collect::<Result<Vec<StoreValue>, _>>()
                    .map_err(|e| e.to_string())?;
                Ok(Some(ImportedSessionView {
                    id: held.id,
                    header: held.header,
                    entries,
                    divergence: divergence_of(&held.divergence),
                }))
            })
            .await
    }

    pub async fn record_projected_tail(
        &self,
        path: &Path,
        session_id: &str,
        tails: &[ProjectedTail],
        written: JsonlFileStamp,
    ) -> Result<(), String> {
        let resolved = Self::resolve_path(path);
        let Some(tail) = tails.last() else {
            return Ok(());
        };
        let end = tail.offset + u64::try_from(tail.line.len()).unwrap_or(u64::MAX);
        if written.size != end + 1 {
            return Err("Projection size does not match its written image".to_owned());
        }
        let previous = self.ledger(&resolved).await?;
        let mut prefix_digest = previous.as_ref().and_then(|p| p.prefix_digest.clone());
        let mut next = previous.as_ref().map(|p| p.high_water_mark.end + 1);
        for each in tails {
            if prefix_digest.is_none() || next != Some(each.offset) {
                prefix_digest = None;
                break;
            }
            if let Some(prev) = prefix_digest.as_deref() {
                prefix_digest = Some(chain_line(prev, &each.line));
            }
            next = Some(each.offset + u64::try_from(each.line.len()).unwrap_or(u64::MAX) + 1);
        }
        let record = JsonlImportLedger {
            size: written.size,
            mtime_ms: written.mtime_ms,
            ctime_ms: written.ctime_ms,
            path: resolved.clone(),
            source_session_id: session_id.to_owned(),
            session_id: session_id.to_owned(),
            high_water_mark: HighWaterMark {
                id: tail.key.clone(),
                hash: hash_short(tail.line.as_bytes()),
                offset: tail.offset,
                end,
            },
            prefix_digest,
            writer_kind: WriterKind::Store,
            checked_at: (self.now)(),
        };
        self.commit(vec![StoreWrite::Put {
            key: ledger_key(&resolved),
            data: serde_json::to_value(record).map_err(|e| e.to_string())?,
        }])
        .await
    }

    pub async fn record_projection(
        &self,
        path: &Path,
        contents: &str,
        written: JsonlFileStamp,
    ) -> Result<(), String> {
        let resolved = Self::resolve_path(path);
        let bytes = contents.as_bytes();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != written.size {
            return Err("Projection size does not match its written image".to_owned());
        }
        let parsed = parse_jsonl(bytes)?;
        let held = self
            .manifest(&parsed.header.key)
            .await?
            .ok_or_else(|| "Projection does not match the committed session".to_owned())?;
        let held_vals: Vec<StoreValue> = held
            .entries
            .iter()
            .filter_map(|e| serde_json::from_str(&e.line).ok())
            .collect();
        let entry_vals: Vec<StoreValue> = parsed.entries.iter().map(|e| e.value.clone()).collect();
        if held_vals != entry_vals {
            return Err("Projection does not match the committed session".to_owned());
        }
        let digest = chain_of(
            std::iter::once(parsed.header.raw.as_str())
                .chain(parsed.entries.iter().map(|r| r.raw.as_str())),
        )
        .last()
        .cloned();
        let record = JsonlImportLedger {
            size: written.size,
            mtime_ms: written.mtime_ms,
            ctime_ms: written.ctime_ms,
            path: resolved.clone(),
            source_session_id: parsed.header.key.clone(),
            session_id: held.id,
            high_water_mark: high_water_mark(parsed.entries.last().unwrap_or(&parsed.header)),
            prefix_digest: digest,
            writer_kind: WriterKind::Store,
            checked_at: (self.now)(),
        };
        self.commit(vec![StoreWrite::Put {
            key: ledger_key(&resolved),
            data: serde_json::to_value(record).map_err(|e| e.to_string())?,
        }])
        .await
    }
}

pub fn create_jsonl_importer(options: JsonlImporterOptions) -> Arc<JsonlImporter> {
    Arc::new(JsonlImporter::new(options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::durable::{
        DurableStoreOptions, create_admission_line, create_memory_store_medium, open_memory_store,
    };

    #[tokio::test]
    async fn imports_extends_and_detects_unchanged_and_hatch_fork() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("sess.jsonl");
        let header = json!({
            "type": "session",
            "version": 3,
            "id": "s1",
            "timestamp": "2026-11-24T00:00:00.000Z",
            "cwd": "/w"
        });
        let m1 = json!({
            "type": "message",
            "id": "m1",
            "parentId": null,
            "timestamp": "2026-11-24T00:00:01.000Z",
            "message": { "role": "user", "content": "hi" }
        });
        let m2 = json!({
            "type": "message",
            "id": "m2",
            "parentId": "m1",
            "timestamp": "2026-11-24T00:00:02.000Z",
            "message": { "role": "assistant", "content": [{"type": "text", "text": "hello"}] }
        });
        tokio::fs::write(&file, format!("{header}\n{m1}\n"))
            .await
            .expect("write");

        let store = Arc::new(open_memory_store(
            create_memory_store_medium(),
            DurableStoreOptions::default(),
        ));
        store.claim(0, 1).await.expect("claim");
        let importer = create_jsonl_importer(JsonlImporterOptions {
            store,
            epoch: 1,
            admission: create_admission_line(),
            files: None,
            now: None,
            mtime_tick_ms: Some(0.0),
        });

        let r1 = importer.import_file(&file).await.expect("import 1");
        assert_eq!(r1.outcome, JsonlImportOutcome::Imported);
        assert_eq!(r1.imported, 1);

        let r2 = importer.import_file(&file).await.expect("import 2");
        assert_eq!(r2.outcome, JsonlImportOutcome::Unchanged);

        tokio::fs::write(&file, format!("{header}\n{m1}\n{m2}\n"))
            .await
            .expect("extend");
        let r3 = importer.import_file(&file).await.expect("import 3");
        assert_eq!(r3.outcome, JsonlImportOutcome::Extended);
        assert_eq!(r3.imported, 1);

        // Diverged with hatch marker -> fork
        let m2_alt = json!({
            "type": "message",
            "id": "m2b",
            "parentId": "m1",
            "timestamp": "2026-11-24T00:00:03.000Z",
            "message": { "role": "assistant", "content": [{"type": "text", "text": "alt"}] }
        });
        tokio::fs::write(&file, format!("{header}\n{m1}\n{m2_alt}\n"))
            .await
            .expect("rewrite");
        let stamp = RealJsonlFiles.stat(&file).await.expect("stat");
        write_hatch_marker(
            &file,
            HatchMarker {
                size: stamp.size,
                mtime_ms: stamp.mtime_ms,
            },
        )
        .await
        .expect("hatch marker");

        let r4 = importer.import_file(&file).await.expect("import 4");
        assert_eq!(r4.outcome, JsonlImportOutcome::Forked);
        assert_ne!(r4.session_id, "s1");
    }
}
