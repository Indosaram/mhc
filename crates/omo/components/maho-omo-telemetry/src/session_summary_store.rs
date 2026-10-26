//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-storage.ts` plus the
//! recovery-only operations of `session-summary-recovery-storage.ts`.
//!
//! `<state>/session-summary` holds one window file per session and `<state>/session-summary/pending` one
//! file per event awaiting acknowledgment. Both are version 2 records carrying the writer's consent mark and
//! consent scope, because a later process must judge a record by its writer's config, never its own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value, json};

use crate::session_role::{TelemetrySessionRole, parse_telemetry_session_role};
use crate::session_summary_files::{
    JsonRead, is_safe_name, is_safe_uuid, list_names, read_json_or_corrupt, remove_file,
    rename_if_present, write_json_atomic,
};
use crate::session_summary_liveness::{PidProbe, create_owner_liveness};
use crate::session_summary_owner::{
    Clock, OwnerRegistry, OwnerRegistryInput, ProcessOwner, ProcessStartProbe, parse_owner,
};
use crate::session_summary_purge::{ConfigSelection, parse_config_selection, summary_dir};
use crate::session_summary_window::{
    SummaryWindow, is_summary_epoch, parse_window, window_to_json,
};

pub const SUMMARY_TTL_MS: f64 = 14.0 * 24.0 * 60.0 * 60_000.0;
pub const OWNER_STALE_MS: f64 = 5.0 * 60_000.0;
const OWNER_ENTRY_REFRESH_MS: f64 = 60_000.0;

/// The consent of the session a queued record came from. Only `enabled` records are ever sent; anything
/// else, including a record written before this mark existed, is deleted unsent (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordConsent {
    Enabled,
    Unconsented,
}

impl RecordConsent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Unconsented => "unconsented",
        }
    }
}

/// Where the writing session read its consent: before a record is sent or resumed, consent is re-read for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsentScope {
    pub cwd: Option<String>,
    pub config: Option<ConfigSelection>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowRecord {
    pub consent: RecordConsent,
    /// Absent on records older than the scope; such a record is never sent or resumed.
    pub scope: Option<ConsentScope>,
    pub session_hash: String,
    pub session_role: TelemetrySessionRole,
    pub owner: ProcessOwner,
    pub heartbeat_at: f64,
    /// The run `seq` counts in; absent on records older than the epoch (a continuer starts a new one).
    pub epoch: Option<String>,
    /// The `summary_seq` the open window will carry, or the next window when none is open.
    pub seq: u64,
    pub sample_rate: f64,
    pub window_id: Option<String>,
    pub window: Option<SummaryWindow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingRecord {
    pub consent: RecordConsent,
    pub scope: Option<ConsentScope>,
    pub uuid: String,
    pub session_hash: String,
    pub owner: ProcessOwner,
    pub created_at: f64,
    pub properties: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReadResult<T> {
    Missing,
    Corrupt,
    Value(T),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Claimed<T> {
    pub claim_path: PathBuf,
    pub record: ReadResult<T>,
}

/// The liveness probes recovery tells a dead owner apart with; absent ones default to the real probes.
#[derive(Clone, Default)]
pub struct Probes {
    pub is_pid_alive: Option<PidProbe>,
    pub process_start_time: Option<ProcessStartProbe>,
}

/// The single send/resume gate for a persisted record: it must carry the `enabled` mark AND its originating
/// scope must still be opted in now. A record whose scope cannot be read back (older file) or names no cwd
/// fails closed: consent read without a cwd would be the reading process's default, not the writer's.
pub fn is_record_sendable(
    consent: RecordConsent,
    scope: Option<&ConsentScope>,
    is_scope_consented: &(dyn Fn(&ConsentScope) -> bool + Send + Sync),
) -> bool {
    if consent != RecordConsent::Enabled {
        return false;
    }
    let Some(scope) = scope else {
        return false;
    };
    match scope.cwd.as_deref() {
        None | Some("") => false,
        Some(_) => is_scope_consented(scope),
    }
}

#[derive(Clone)]
pub struct SummaryStorage {
    pub dir: PathBuf,
    pub owner: ProcessOwner,
    pub now: Clock,
    pub probes: Probes,
    registered_at: Arc<Mutex<Option<f64>>>,
}

pub fn default_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    })
}

pub fn create_summary_storage(
    state_dir: &Path,
    owner: ProcessOwner,
    probes: Probes,
    now: Option<Clock>,
) -> SummaryStorage {
    SummaryStorage {
        dir: summary_dir(state_dir),
        owner,
        now: now.unwrap_or_else(default_clock),
        probes,
        registered_at: Arc::new(Mutex::new(None)),
    }
}

impl SummaryStorage {
    fn pending_dir(&self) -> PathBuf {
        self.dir.join("pending")
    }

    fn window_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn pending_path(&self, uuid: &str) -> PathBuf {
        self.pending_dir().join(format!("{uuid}.json"))
    }

    fn registry(&self) -> OwnerRegistry {
        OwnerRegistry::new(OwnerRegistryInput {
            dir: self.dir.clone(),
            owner: self.owner.clone(),
            process_start_time: self.probes.process_start_time.clone(),
            now: Some(Arc::clone(&self.now)),
        })
    }

    /// Writes (or refreshes, at most once a minute) this process's owner entry.
    pub fn register(&self) {
        let mut registered_at = self
            .registered_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = (self.now)();
        if registered_at.is_some_and(|at| now - at < OWNER_ENTRY_REFRESH_MS) {
            return;
        }
        *registered_at = Some(now);
        drop(registered_at);
        self.registry().register();
    }

    pub fn window_key(&self, session_hash: &str, own_copy: bool) -> String {
        let base = file_name(session_hash);
        if own_copy {
            format!("{base}@{}", self.owner.boot_id)
        } else {
            base
        }
    }

    pub fn read_window(&self, key: &str) -> ReadResult<WindowRecord> {
        parse_read(read_json_or_corrupt(&self.window_path(key)), parse_window_record)
    }

    pub fn write_window(&self, key: &str, record: &WindowRecord) -> std::io::Result<()> {
        self.register();
        write_json_atomic(&self.window_path(key), &window_record_to_json(record), &self.owner)
    }

    pub fn remove_window(&self, key: &str) {
        remove_file(&self.window_path(key));
    }

    pub fn window_keys(&self) -> Vec<String> {
        json_keys(&self.dir)
    }

    pub fn write_pending(&self, record: &PendingRecord) -> std::io::Result<()> {
        self.register();
        write_json_atomic(
            &self.pending_path(&record.uuid),
            &pending_record_to_json(record),
            &self.owner,
        )
    }

    pub fn remove_pending(&self, uuid: &str) {
        remove_file(&self.pending_path(uuid));
    }

    pub fn pending_uuids(&self) -> Vec<String> {
        json_keys(&self.pending_dir())
    }

    pub fn pending_bytes(&self, uuid: &str) -> u64 {
        std::fs::metadata(self.pending_path(uuid)).map_or(0, |meta| meta.len())
    }
}

fn json_keys(dir: &Path) -> Vec<String> {
    let mut keys: Vec<String> = list_names(dir)
        .into_iter()
        .filter_map(|name| name.strip_suffix(".json").map(str::to_string))
        .collect();
    keys.sort();
    keys
}

pub fn parse_read<T>(
    value: JsonRead,
    parse: impl Fn(&Value) -> Option<T>,
) -> ReadResult<T> {
    match value {
        JsonRead::Missing => ReadResult::Missing,
        JsonRead::Corrupt => ReadResult::Corrupt,
        JsonRead::Value(value) => match parse(&value) {
            Some(parsed) => ReadResult::Value(parsed),
            None => ReadResult::Corrupt,
        },
    }
}

pub fn parse_window_record(value: &Value) -> Option<WindowRecord> {
    if value.get("v").and_then(Value::as_u64) != Some(2) {
        return None;
    }
    let owner = parse_owner(value.get("owner")?)?;
    let session_hash = value.get("sessionHash")?.as_str()?.to_string();
    let heartbeat_at = value.get("heartbeatAt")?.as_f64()?;
    let seq = value.get("seq")?.as_u64()?;
    let sample_rate = value.get("sampleRate")?.as_f64()?;
    let window_id = value.get("windowId").and_then(Value::as_str);
    let window = value.get("window").filter(|window| !window.is_null());
    let window_is_open = window
        .and_then(parse_window)
        .is_some_and(|_| window_id.is_some_and(is_safe_uuid));
    if !window_is_open && window.is_some() {
        return None;
    }
    Some(WindowRecord {
        consent: parse_consent(value.get("consent")),
        scope: parse_scope(value.get("scope")),
        session_hash,
        session_role: parse_telemetry_session_role(
            value.get("sessionRole").and_then(Value::as_str),
        ),
        owner,
        heartbeat_at,
        epoch: value
            .get("epoch")
            .and_then(Value::as_str)
            .filter(|epoch| is_summary_epoch(epoch))
            .map(str::to_string),
        seq,
        sample_rate,
        window_id: window_is_open.then(|| window_id.unwrap_or_default().to_string()),
        window: window_is_open.then(|| window.and_then(parse_window)).flatten(),
    })
}

pub fn parse_pending_record(value: &Value) -> Option<PendingRecord> {
    if value.get("v").and_then(Value::as_u64) != Some(2) {
        return None;
    }
    let owner = parse_owner(value.get("owner")?)?;
    let uuid = value.get("uuid")?.as_str()?;
    if !is_safe_uuid(uuid) {
        return None;
    }
    let session_hash = value.get("sessionHash")?.as_str()?.to_string();
    let created_at = value.get("createdAt")?.as_f64()?;
    let properties = value.get("properties")?.as_object()?;
    Some(PendingRecord {
        consent: parse_consent(value.get("consent")),
        scope: parse_scope(value.get("scope")),
        uuid: uuid.to_string(),
        session_hash,
        owner,
        created_at,
        properties: properties.clone(),
    })
}

pub fn window_record_to_json(record: &WindowRecord) -> Value {
    let mut object = Map::new();
    object.insert("v".into(), json!(2));
    object.insert("consent".into(), json!(record.consent.as_str()));
    if let Some(scope) = &record.scope {
        object.insert("scope".into(), scope_to_json(scope));
    }
    object.insert("sessionHash".into(), json!(record.session_hash));
    object.insert("sessionRole".into(), json!(record.session_role.as_str()));
    object.insert(
        "owner".into(),
        json!({"pid": record.owner.pid, "bootId": record.owner.boot_id}),
    );
    object.insert("heartbeatAt".into(), json!(record.heartbeat_at));
    if let Some(epoch) = &record.epoch {
        object.insert("epoch".into(), json!(epoch));
    }
    object.insert("seq".into(), json!(record.seq));
    object.insert("sampleRate".into(), json!(record.sample_rate));
    match (&record.window_id, &record.window) {
        (Some(window_id), Some(window)) => {
            object.insert("windowId".into(), json!(window_id));
            object.insert("window".into(), window_to_json(window));
        }
        _ => {
            object.insert("windowId".into(), Value::Null);
            object.insert("window".into(), Value::Null);
        }
    }
    Value::Object(object)
}

pub fn pending_record_to_json(record: &PendingRecord) -> Value {
    let mut object = Map::new();
    object.insert("v".into(), json!(2));
    object.insert("consent".into(), json!(record.consent.as_str()));
    if let Some(scope) = &record.scope {
        object.insert("scope".into(), scope_to_json(scope));
    }
    object.insert("uuid".into(), json!(record.uuid));
    object.insert("sessionHash".into(), json!(record.session_hash));
    object.insert(
        "owner".into(),
        json!({"pid": record.owner.pid, "bootId": record.owner.boot_id}),
    );
    object.insert("createdAt".into(), json!(record.created_at));
    object.insert("properties".into(), Value::Object(record.properties.clone()));
    Value::Object(object)
}

pub fn scope_to_json(scope: &ConsentScope) -> Value {
    let mut object = Map::new();
    if let Some(cwd) = &scope.cwd {
        object.insert("cwd".into(), json!(cwd));
    }
    if let Some(config) = &scope.config {
        object.insert("config".into(), json!(config));
    }
    Value::Object(object)
}

fn parse_consent(value: Option<&Value>) -> RecordConsent {
    match value.and_then(Value::as_str) {
        Some("enabled") => RecordConsent::Enabled,
        _ => RecordConsent::Unconsented,
    }
}

fn parse_scope(value: Option<&Value>) -> Option<ConsentScope> {
    let object = value?.as_object()?;
    let raw_config = object.get("config");
    let config = match raw_config {
        None => None,
        Some(raw) => match parse_config_selection(raw) {
            Some(config) => Some(config),
            None => return None,
        },
    };
    let cwd = match object.get("cwd") {
        None => None,
        Some(raw) => Some(raw.as_str()?.to_string()),
    };
    Some(ConsentScope { cwd, config })
}

fn file_name(session_hash: &str) -> String {
    if is_safe_name(session_hash) {
        return session_hash.replace(':', "_");
    }
    session_hash
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(128)
        .collect()
}

/// The storage operations only recovery uses: owner liveness, exclusive create, claims and leftovers.
pub struct RecoveryStorage {
    pub storage: SummaryStorage,
}

pub fn with_recovery_ops(storage: SummaryStorage) -> RecoveryStorage {
    RecoveryStorage { storage }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftoverKind {
    Temp,
    WindowClaim,
    PendingClaim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub path: PathBuf,
    pub owner: ProcessOwner,
    pub kind: LeftoverKind,
}

impl RecoveryStorage {
    pub fn is_owner_alive(&self) -> Arc<dyn Fn(&ProcessOwner) -> bool + Send + Sync> {
        create_owner_liveness(
            &OwnerRegistryInput {
                dir: self.storage.dir.clone(),
                owner: self.storage.owner.clone(),
                process_start_time: self.storage.probes.process_start_time.clone(),
                now: Some(Arc::clone(&self.storage.now)),
            },
            self.storage.probes.is_pid_alive.clone(),
        )
    }

    /// A key one process wrote beside another process's window; nobody continues it once its writer is gone.
    pub fn is_own_copy_key(&self, key: &str) -> bool {
        key.contains('@')
    }

    /// Exclusive create: never replaces a window another process wrote in the meantime.
    pub fn create_window(&self, key: &str, record: &WindowRecord) -> bool {
        self.storage.register();
        let target = self.storage.window_path(key);
        if let Some(parent) = target.parent() {
            let _ignored = std::fs::create_dir_all(parent);
        }
        let temp = crate::session_summary_files::temp_path(&target, &self.storage.owner);
        let written = crate::session_summary_files::write_json_atomic(
            &temp,
            &window_record_to_json(record),
            &self.storage.owner,
        );
        if written.is_err() {
            return false;
        }
        let created = crate::session_summary_files::link_if_absent(&temp, &target);
        remove_file(&temp);
        created
    }

    pub fn claim_window(&self, key: &str) -> Option<Claimed<WindowRecord>> {
        let path = self.storage.window_path(key);
        claim_file(&path, &self.storage.owner, parse_window_record)
    }

    /// Whether the window's event is pending, or claimed by a process resending it.
    pub fn has_pending(&self, uuid: &str) -> bool {
        let prefix = format!("{uuid}.json");
        list_names(&self.storage.pending_dir())
            .into_iter()
            .any(|name| name == prefix || name.starts_with(&format!("{prefix}.claimed-")))
    }

    pub fn read_pending(&self, uuid: &str) -> ReadResult<PendingRecord> {
        parse_read(
            read_json_or_corrupt(&self.storage.pending_path(uuid)),
            parse_pending_record,
        )
    }

    pub fn claim_pending(&self, uuid: &str) -> Option<Claimed<PendingRecord>> {
        let path = self.storage.pending_path(uuid);
        claim_file(&path, &self.storage.owner, parse_pending_record)
    }

    pub fn leftovers(&self) -> Vec<Leftover> {
        let mut leftovers = collect_leftovers(&self.storage.dir, LeftoverKind::WindowClaim);
        leftovers.extend(collect_leftovers(
            &self.storage.pending_dir(),
            LeftoverKind::PendingClaim,
        ));
        leftovers
    }
}

/// Hands `path` over to `owner` by renaming it; exactly one of several racing claimers gets it.
fn claim_file<T>(
    path: &Path,
    owner: &ProcessOwner,
    parse: impl Fn(&Value) -> Option<T>,
) -> Option<Claimed<T>> {
    let claim_path = PathBuf::from(format!(
        "{}.claimed-{}-{}",
        path.display(),
        owner.pid,
        owner.boot_id
    ));
    if !rename_if_present(path, &claim_path) {
        return None;
    }
    Some(Claimed {
        record: parse_read(read_json_or_corrupt(&claim_path), parse),
        claim_path,
    })
}

fn collect_leftovers(dir: &Path, claim_kind: LeftoverKind) -> Vec<Leftover> {
    list_names(dir)
        .into_iter()
        .filter_map(|name| {
            let (pid, boot_id, is_claim) = split_leftover_name(&name)?;
            let owner = parse_owner(&json!({"pid": pid, "bootId": boot_id}))?;
            Some(Leftover {
                path: dir.join(&name),
                owner,
                kind: if is_claim {
                    claim_kind
                } else {
                    LeftoverKind::Temp
                },
            })
        })
        .collect()
}

/// Splits `<base>.<pid>-<bootId>.tmp` and `<base>.claimed-<pid>-<bootId>` into their owner parts.
fn split_leftover_name(name: &str) -> Option<(u32, String, bool)> {
    let (stem, is_claim) = match name.strip_suffix(".tmp") {
        Some(stem) => (stem, false),
        None => (name, true),
    };
    let marker = if is_claim { ".claimed-" } else { "." };
    let index = stem.rfind(marker)?;
    let owner = &stem[index + marker.len()..];
    let (pid, boot_id) = owner.split_once('-')?;
    let pid: u32 = pid.parse().ok()?;
    if boot_id.is_empty() || boot_id.len() > 64 {
        return None;
    }
    if !boot_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    Some((pid, boot_id.to_string(), is_claim))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_window::{EndedBy, open_window};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn owner(pid: u32, boot_id: &str) -> ProcessOwner {
        ProcessOwner {
            pid,
            boot_id: boot_id.into(),
        }
    }

    fn storage(dir: &Path) -> SummaryStorage {
        create_summary_storage(
            dir,
            owner(1, "boot-self"),
            Probes::default(),
            Some(Arc::new(|| 1000.0)),
        )
    }

    fn window_record() -> WindowRecord {
        WindowRecord {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            session_hash: "a".repeat(64),
            session_role: TelemetrySessionRole::Root,
            owner: owner(1, "boot-self"),
            heartbeat_at: 1000.0,
            epoch: Some("0123456789abcdef".into()),
            seq: 3,
            sample_rate: 0.05,
            window_id: Some("01234567-89ab-cdef-0123-456789abcdef".into()),
            window: Some(open_window(900.0, 1.0)),
        }
    }

    #[test]
    fn window_and_pending_records_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let key = storage.window_key(&"a".repeat(64), false);
        storage.write_window(&key, &window_record()).unwrap();
        assert_eq!(storage.window_keys(), vec![key.clone()]);
        let read = storage.read_window(&key);
        assert_eq!(read, ReadResult::Value(window_record()));
        // session-summary-storage.ts nests the owner registry inside summaryDir(stateDir).
        assert!(storage.dir.join("owners/1.json").exists());

        let pending = PendingRecord {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: Some(ConfigSelection::from([(
                    "OMO_PROFILE".to_string(),
                    "work".to_string(),
                )])),
            }),
            uuid: "01234567-89ab-cdef-0123-456789abcdef".into(),
            session_hash: "a".repeat(64),
            owner: owner(1, "boot-self"),
            created_at: 1000.0,
            properties: Map::from_iter([("turns".to_string(), json!(2))]),
        };
        storage.write_pending(&pending).unwrap();
        assert_eq!(storage.pending_uuids(), vec![pending.uuid.clone()]);
        assert_eq!(
            with_recovery_ops(storage.clone()).read_pending(&pending.uuid),
            ReadResult::Value(pending.clone())
        );
        assert!(storage.pending_bytes(&pending.uuid) > 0);
    }

    #[test]
    fn corrupt_and_unknown_versions_read_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let key = "broken";
        std::fs::create_dir_all(&storage.dir).unwrap();
        std::fs::write(storage.dir.join("broken.json"), "{not json").unwrap();
        assert_eq!(storage.read_window(key), ReadResult::Corrupt);
        std::fs::write(storage.dir.join("broken.json"), json!({"v": 1}).to_string()).unwrap();
        assert_eq!(storage.read_window(key), ReadResult::Corrupt);
        std::fs::write(
            storage.dir.join("broken.json"),
            json!({"v":2,"owner":{"pid":1,"bootId":"b"},"sessionHash":"h","heartbeatAt":0.0,"seq":0,"sampleRate":0.05,"windowId":"nope","window":{"startedAt":0,"lastAt":0,"firstTurnIndex":0,"lastTurnIndex":0,"sums":{},"totals":[],"models":[]}}).to_string(),
        )
        .unwrap();
        assert_eq!(storage.read_window(key), ReadResult::Corrupt);
        assert_eq!(storage.read_window("absent"), ReadResult::Missing);
    }

    #[test]
    fn own_copies_get_a_distinct_key_and_unnamed_scopes_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        assert_eq!(
            storage.window_key(&"a".repeat(64), true),
            format!("{}@boot-self", "a".repeat(64))
        );
        assert_eq!(
            storage.window_key("with:colon", false),
            "with_colon".to_string()
        );
        assert_eq!(storage.window_key("with/slash", false).len(), 20);
        let consented = |_: &ConsentScope| true;
        assert!(!is_record_sendable(RecordConsent::Unconsented, Some(&ConsentScope::default()), &consented));
        assert!(!is_record_sendable(RecordConsent::Enabled, None, &consented));
        assert!(!is_record_sendable(
            RecordConsent::Enabled,
            Some(&ConsentScope::default()),
            &consented
        ));
        assert!(is_record_sendable(
            RecordConsent::Enabled,
            Some(&ConsentScope {
                cwd: Some("/repo".into()),
                config: None
            }),
            &consented
        ));
    }

    #[test]
    fn owner_entry_is_refreshed_at_most_once_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let ticks = Arc::new(Mutex::new(0.0));
        let clock = Arc::clone(&ticks);
        let storage = create_summary_storage(
            dir.path(),
            owner(1, "boot-self"),
            Probes::default(),
            Some(Arc::new(move || *clock.lock().unwrap())),
        );
        storage.register();
        let first: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("session-summary/owners/1.json")).unwrap(),
        )
        .unwrap();
        // A clock read must not advance time: registration itself also reads the clock.
        *ticks.lock().unwrap() = 30_000.0;
        storage.register();
        let second: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("session-summary/owners/1.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(first["heartbeatAt"], second["heartbeatAt"]);
        *ticks.lock().unwrap() = 60_000.0;
        storage.register();
        let third: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("session-summary/owners/1.json")).unwrap(),
        )
        .unwrap();
        assert_ne!(first["heartbeatAt"], third["heartbeatAt"]);
    }

    #[test]
    fn recovery_ops_claim_exclusively_and_report_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let key = storage.window_key("window-key", false);
        storage.write_window(&key, &window_record()).unwrap();
        let recovery = with_recovery_ops(storage.clone());
        assert!(recovery.claim_window(&key).is_some());
        assert!(recovery.claim_window(&key).is_none());
        assert_eq!(recovery.storage.window_keys(), Vec::<String>::new());
        let leftovers = recovery.leftovers();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].kind, LeftoverKind::WindowClaim);
        assert_eq!(leftovers[0].owner, owner(1, "boot-self"));
    }

    #[test]
    fn exclusive_create_never_replaces_a_peer_window() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let recovery = with_recovery_ops(storage.clone());
        let key = "peer-key";
        assert!(recovery.create_window(key, &window_record()));
        assert!(!recovery.create_window(key, &window_record()));
        assert!(matches!(storage.read_window(key), ReadResult::Value(_)));
    }

    #[test]
    fn pending_presence_covers_claims_and_liveness_agrees() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let recovery = with_recovery_ops(storage.clone());
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        assert!(!recovery.has_pending(uuid));
        std::fs::create_dir_all(storage.dir.join("pending")).unwrap();
        std::fs::write(
            storage.dir.join(format!("pending/{uuid}.json.claimed-9-b")),
            "{}",
        )
        .unwrap();
        assert!(recovery.has_pending(uuid));
        let alive = recovery.is_owner_alive();
        assert!(alive(&owner(1, "boot-self")));
        let flag = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&flag);
        let storage = create_summary_storage(
            dir.path(),
            owner(1, "boot-self"),
            Probes {
                is_pid_alive: Some(Arc::new(move |_| observed.load(Ordering::SeqCst))),
                process_start_time: Some(Arc::new(|_| None)),
            },
            Some(Arc::new(|| 1000.0)),
        );
        let recovery = with_recovery_ops(storage);
        let alive = recovery.is_owner_alive();
        assert!(!alive(&owner(2, "other")));
    }

    #[test]
    fn leftover_names_parse_only_owner_qualified_entries() {
        assert_eq!(
            split_leftover_name("window.json.42-boot-a.tmp"),
            Some((42, "boot-a".into(), false))
        );
        assert_eq!(
            split_leftover_name("window.json.claimed-42-boot-a"),
            Some((42, "boot-a".into(), true))
        );
        for invalid in ["window.json", "window.json.tmp", "window.json.x-b.tmp", "a.b.json"] {
            assert_eq!(split_leftover_name(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn ttl_and_stale_constants_match_the_specification() {
        assert_eq!(SUMMARY_TTL_MS, 14.0 * 24.0 * 60.0 * 60_000.0);
        assert_eq!(OWNER_STALE_MS, 5.0 * 60_000.0);
        assert_eq!(EndedBy::Recovered.as_str(), "recovered");
    }
}
