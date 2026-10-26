use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolCreate, WorkpoolError, WorkpoolErrorCode, WorkpoolMode, WorkpoolRecord, WorkpoolSpec};

#[derive(Clone)]
pub struct WorkpoolStore {
    state_dir: PathBuf,
    gate: Arc<Mutex<()>>,
    mutation_listener: MutationListener,
}
type MutationListener = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

impl Default for WorkpoolStore {
    fn default() -> Self { Self::new(PathBuf::new()) }
}

impl WorkpoolStore {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self { Self { state_dir: state_dir.into(), gate: Arc::new(Mutex::new(())), mutation_listener: Arc::default() } }
    pub fn set_mutation_listener(&self, listener: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.mutation_listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = listener;
    }
    pub fn state_dir(&self) -> &Path { &self.state_dir }

    fn directory(&self) -> PathBuf { self.state_dir.join("workpools") }
    fn path(&self, id: &str) -> Result<PathBuf, WorkpoolError> {
        validate_id(id)?;
        Ok(self.directory().join(format!("{id}.json")))
    }

    pub fn load(&self, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        let _guard = self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.load_unlocked(pool_id)
    }

    fn load_unlocked(&self, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        let path = self.path(pool_id)?;
        let mut bytes = String::new();
        match File::open(&path) {
            Ok(mut file) => { file.read_to_string(&mut bytes).map_err(|e| io_error("Could not read pool record.", e))?; },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(error(WorkpoolErrorCode::PoolNotFound, "Pool not found.")),
            Err(e) => return Err(io_error("Could not read pool record.", e)),
        }
        let value: Value = serde_json::from_str(&bytes).map_err(|e| corrupt(e.to_string()))?;
        super::record_schema::validate_record(&value).map_err(|e| corrupt(e.to_string()))?;
        let disk: DiskRecord = serde_json::from_value(value).map_err(|e| corrupt(e.to_string()))?;
        let mut record = disk.into_record()?;
        let sidecar = read_pause_sidecar(&self.directory().join(format!("{pool_id}.pause.json")))?;
        if let Some(pause) = sidecar.revisions.get(&record.revision.to_string()) { with_pause(&mut record, pause); }
        if record.pool_id != pool_id { return Err(corrupt("Pool identity does not match its file.")); }
        Ok(record)
    }

    pub fn list(&self) -> Vec<WorkpoolRecord> { self.list_result().expect("workpool store listing failed") }

    pub fn list_result(&self) -> Result<Vec<WorkpoolRecord>, WorkpoolError> {
        let _guard = self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.list_unlocked()
    }

    fn list_unlocked(&self) -> Result<Vec<WorkpoolRecord>, WorkpoolError> {
        let entries = match fs::read_dir(self.directory()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error("Could not list workpool directory.", e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_error("Could not read workpool directory entry.", e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() == 40 && name.starts_with("wp_") && name.ends_with(".json")
                && name[3..35].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { names.push(name); }
        }
        names.sort();
        names.into_iter().map(|name| self.load_unlocked(&name[..35])).collect()
    }

    pub fn owned(&self, caller: &WorkpoolCaller, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        let record = self.load(pool_id)?;
        if record.parent_session_id != caller.session_id { return Err(error(WorkpoolErrorCode::ScopeDenied, "Only the owning parent session may access this pool.")); }
        Ok(record)
    }

    pub fn mutate(&self, pool_id: &str, update: impl FnOnce(&WorkpoolRecord) -> WorkpoolRecord) -> Result<WorkpoolRecord, WorkpoolError> {
        self.mutate_result(pool_id, |r| Ok(update(r)))
    }

    pub fn mutate_result(&self, pool_id: &str, update: impl FnOnce(&WorkpoolRecord) -> Result<WorkpoolRecord, WorkpoolError>) -> Result<WorkpoolRecord, WorkpoolError> {
        let _guard = self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.path(pool_id)?;
        let prior = self.load_unlocked(pool_id)?;
        let next = update(&prior)?;
        if same_record(&next, &prior) { return Ok(prior); }
        let mut saved = next;
        saved.revision = prior.revision.saturating_add(1);
        write_pool(&path, &saved)?;
        drop(_guard);
        let listener = self.mutation_listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(listener) = listener { listener(); }
        Ok(saved)
    }

    pub fn create(&self, caller: &WorkpoolCaller, input: &WorkpoolCreate, mode: WorkpoolMode, worker_spec: WorkpoolSpec) -> Result<WorkpoolRecord, WorkpoolError> {
        let _guard = self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = self.directory();
        fs::create_dir_all(&directory).map_err(|e| io_error("Could not create workpool directory.", e))?;
        let lock_path = directory.join("names.lock");
        let lock = open_lock(&lock_path)?;
        lock_file(&lock)?;
        let existing = self.list_unlocked()?.into_iter().find(|p| p.parent_session_id == caller.session_id && p.name == input.name);
        if let Some(existing) = existing {
            let same = canonical(&serde_json::to_value(&existing.agent).map_err(|e| corrupt(e.to_string()))?) == canonical(&serde_json::to_value(&input.agent).map_err(|e| corrupt(e.to_string()))?)
                && existing.mode == mode
                && canonical(&serde_json::to_value(&existing.worker_spec.start).map_err(|e| corrupt(e.to_string()))?) == canonical(&serde_json::to_value(&worker_spec.start).map_err(|e| corrupt(e.to_string()))?)
                && canonical(&serde_json::to_value(&existing.worker_spec.plan).map_err(|e| corrupt(e.to_string()))?) == canonical(&serde_json::to_value(&worker_spec.plan).map_err(|e| corrupt(e.to_string()))?)
                && same_grant(existing.kernel_tool_names.as_deref(), input.tools.as_deref());
            if !same || existing.status != super::types::WorkpoolStatus::Open { return Err(error(WorkpoolErrorCode::PoolNameConflict, "Pool name is already in use; choose a new explicit name.")); }
            return Ok(existing);
        }
        let id = format!("wp_{}", random_hex()?);
        let record = WorkpoolRecord { hold: None, version: 1, pool_id: id, name: input.name.clone(), parent_session_id: caller.session_id.clone(), root_session_id: caller.root_session_id.clone(), generation: 1, revision: 0, mode, agent: input.agent.clone(), worker_spec, status: super::types::WorkpoolStatus::Open, kernel_tool_names: input.tools.as_ref().filter(|v| !v.is_empty()).cloned(), items: vec![], workers: vec![], aggregate: None };
        let path = self.path(&record.pool_id)?;
        write_pool(&path, &record)?;
        unlock_file(&lock);
        Ok(record)
    }
}

#[derive(Serialize, Deserialize)]
struct DiskRecord {
    version: u8, pool_id: String, name: String, parent_session_id: String, root_session_id: String,
    generation: u32, revision: u32, mode: WorkpoolMode, agent: WorkpoolAgent,
    worker_spec: Value, status: super::types::WorkpoolStatus,
    #[serde(skip_serializing_if = "Option::is_none", default)] kernel_tool_names: Option<Vec<String>>,
    items: Vec<super::types::WorkpoolItem>, workers: Vec<super::types::WorkpoolWorker>,
    #[serde(skip_serializing_if = "Option::is_none", default)] hold: Option<super::types::WorkpoolHold>,
    #[serde(skip_serializing_if = "Option::is_none", default)] aggregate: Option<super::types::WorkpoolAggregate>,
}

impl DiskRecord {
    fn from_record(r: &WorkpoolRecord) -> Result<Self, WorkpoolError> {
        Ok(Self { hold:r.hold, version:r.version, pool_id:r.pool_id.clone(), name:r.name.clone(), parent_session_id:r.parent_session_id.clone(), root_session_id:r.root_session_id.clone(), generation:r.generation, revision:r.revision, mode:r.mode, agent:r.agent.clone(), worker_spec:serde_json::json!({"start":r.worker_spec.start,"plan":r.worker_spec.plan}), status:r.status, kernel_tool_names:r.kernel_tool_names.clone(), items:r.items.clone(), workers:r.workers.clone(), aggregate:r.aggregate })
    }
    fn into_record(self) -> Result<WorkpoolRecord, WorkpoolError> {
        let mut spec = self.worker_spec;
        let start = serde_json::from_value(spec.get_mut("start").map(Value::take).ok_or_else(|| corrupt("Missing worker spec start."))?).map_err(|e| corrupt(e.to_string()))?;
        let plan = serde_json::from_value(spec.get_mut("plan").map(Value::take).ok_or_else(|| corrupt("Missing worker spec plan."))?).map_err(|e| corrupt(e.to_string()))?;
        Ok(WorkpoolRecord { hold: self.hold, version:self.version, pool_id:self.pool_id, name:self.name, parent_session_id:self.parent_session_id, root_session_id:self.root_session_id, generation:self.generation, revision:self.revision, mode:self.mode, agent:self.agent, worker_spec:WorkpoolSpec { start, plan }, status:self.status, kernel_tool_names:self.kernel_tool_names, items:self.items, workers:self.workers, aggregate:self.aggregate })
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseSidecar { revisions: std::collections::BTreeMap<String, PauseState> }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hold: Option<super::types::WorkpoolHold>,
    paused_workers: Vec<String>,
    entry_ids: std::collections::BTreeMap<String, Vec<String>>,
}
fn read_pause_sidecar(path: &Path) -> Result<PauseSidecar, WorkpoolError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PauseSidecar::default()),
        Err(e) => return Err(io_error("Could not read pause sidecar.", e)),
    };
    let sidecar: PauseSidecar = serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
    if sidecar.revisions.values().any(|p| p.hold.as_ref().is_some_and(|h| h.pause_seq == 0)) { return Err(corrupt("Invalid pause sequence.")); }
    Ok(sidecar)
}
fn split_pause(record: &WorkpoolRecord) -> (WorkpoolRecord, Option<PauseState>) {
    let mut base = record.clone();
    let mut pause = PauseState { hold: base.hold.take(), paused_workers: vec![], entry_ids: Default::default() };
    for worker in &mut base.workers {
        if worker.status == super::types::WorkpoolWorkerStatus::Paused {
            pause.paused_workers.push(worker.task_id.clone());
            worker.status = super::types::WorkpoolWorkerStatus::Busy;
        }
    }
    for item in &mut base.items {
        if let Some(delivery) = &mut item.delivery
            && let Some(ids) = delivery.entry_ids.take() { pause.entry_ids.insert(item.item_id.clone(), ids); }
    }
    let present = pause.hold.is_some() || !pause.paused_workers.is_empty() || !pause.entry_ids.is_empty();
    (base, present.then_some(pause))
}
fn with_pause(record: &mut WorkpoolRecord, pause: &PauseState) {
    if pause.hold.is_some() { record.hold = pause.hold; }
    for worker in &mut record.workers {
        if pause.paused_workers.contains(&worker.task_id) { worker.status = super::types::WorkpoolWorkerStatus::Paused; }
    }
    for item in &mut record.items {
        if let (Some(delivery), Some(ids)) = (&mut item.delivery, pause.entry_ids.get(&item.item_id)) { delivery.entry_ids = Some(ids.clone()); }
    }
}
fn write_pool(path: &Path, record: &WorkpoolRecord) -> Result<(), WorkpoolError> {
    let (base, pause) = split_pause(record);
    if record.hold.as_ref().is_some_and(|h| h.pause_seq == 0) { return Err(corrupt("Invalid pause sequence.")); }
    let pause_path = path.with_extension("pause.json");
    let mut prior = read_pause_sidecar(&pause_path)?;
    let had_prior = !prior.revisions.is_empty();
    prior.revisions.retain(|revision, _| record.revision.checked_sub(1).is_some_and(|previous| revision == &previous.to_string()));
    if let Some(pause) = pause { prior.revisions.insert(record.revision.to_string(), pause); }
    if had_prior || !prior.revisions.is_empty() { write_record(&pause_path, &prior)?; }
    write_record(path, &DiskRecord::from_record(&base)?)
}

fn error(code: WorkpoolErrorCode, message: &str) -> WorkpoolError { WorkpoolError::new(code, message) }
fn corrupt(message: impl AsRef<str>) -> WorkpoolError { error(WorkpoolErrorCode::StoreCorrupt, message.as_ref()) }
fn io_error(message: &str, e: std::io::Error) -> WorkpoolError { error(WorkpoolErrorCode::StoreCorrupt, &format!("{message} {e}")) }
fn validate_id(id: &str) -> Result<(), WorkpoolError> {
    if id.len() == 35 && id.starts_with("wp_") && id[3..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { Ok(()) }
    else { Err(error(WorkpoolErrorCode::InvalidPoolId, "Expected a wp_ pool identifier.")) }
}
fn canonical(value: &Value) -> String { match value { Value::Object(m) => { let mut keys: Vec<_> = m.keys().collect(); keys.sort(); format!("{{{}}}", keys.into_iter().map(|k| format!("{}:{}", serde_json::to_string(k).unwrap_or_default(), canonical(&m[k]))).collect::<Vec<_>>().join(",")) }, Value::Array(a) => format!("[{}]",a.iter().map(canonical).collect::<Vec<_>>().join(",")), _ => value.to_string() } }
fn same_grant(a: Option<&[String]>, b: Option<&[String]>) -> bool { let mut a=a.unwrap_or_default().to_vec();let mut b=b.unwrap_or_default().to_vec();a.sort();a.dedup();b.sort();b.dedup();a==b }
fn same_record(a: &WorkpoolRecord,b: &WorkpoolRecord) -> bool {
    a.version==b.version && a.pool_id==b.pool_id && a.name==b.name && a.parent_session_id==b.parent_session_id
        && a.root_session_id==b.root_session_id && a.generation==b.generation && a.revision==b.revision
        && a.mode==b.mode && a.status==b.status && a.items==b.items && a.workers==b.workers
        && a.hold==b.hold && a.kernel_tool_names==b.kernel_tool_names && a.aggregate==b.aggregate
        && (a.agent==b.agent || same_json(&a.agent,&b.agent))
        && same_json(&a.worker_spec.start,&b.worker_spec.start)
        && (a.worker_spec.plan==b.worker_spec.plan || same_json(&a.worker_spec.plan,&b.worker_spec.plan))
}
/// Byte-identical serialization is the common no-op case; key-order differences still fall back to the canonical compare.
fn same_json<T: Serialize + ?Sized>(a: &T, b: &T) -> bool {
    if let (Ok(left), Ok(right)) = (serde_json::to_vec(a), serde_json::to_vec(b)) && left == right { return true; }
    canonical(&serde_json::to_value(a).unwrap_or(Value::Null))==canonical(&serde_json::to_value(b).unwrap_or(Value::Null))
}
fn random_hex() -> Result<String, WorkpoolError> { Ok(uuid::Uuid::new_v4().simple().to_string()) }
fn write_record<T: Serialize>(path: &Path, record: &T) -> Result<(), WorkpoolError> {
    let parent=path.parent().ok_or_else(||corrupt("Pool path has no parent."))?;
    let temp=parent.join(format!(".tmp-{}",random_hex()?));
    let bytes=serde_json::to_vec(record).map_err(|e|corrupt(e.to_string()))?;
    let result=(|| {
        let mut file=OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e|io_error("Could not create temporary pool record.",e))?;
        file.write_all(&bytes).map_err(|e|io_error("Could not write pool record.",e))?;
        file.sync_all().map_err(|e|io_error("Could not sync pool record.",e))?;
        fs::rename(&temp,path).map_err(|e|io_error("Could not replace pool record.",e))?;
        #[cfg(unix)]
        File::open(parent).and_then(|f|f.sync_all()).map_err(|e|io_error("Could not sync pool directory.",e))?;
        Ok::<(), WorkpoolError>(())
    })();
    if result.is_err(){let _=fs::remove_file(temp);} result
}
fn open_lock(path:&Path)->Result<File,WorkpoolError>{OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).map_err(|e|io_error("Could not open workpool lock.",e))}
fn lock_file(f:&File)->Result<(),WorkpoolError>{f.lock().map_err(|e|io_error("Could not lock workpool names.",e))}
fn unlock_file(f:&File){let _=f.unlock();}

pub type WorkpoolStoreHandle = WorkpoolStore;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use crate::manager::types::{ManagerStartSpec, ResolvedChildPlan};
    fn caller() -> WorkpoolCaller { WorkpoolCaller { session_id:"parent".into(),root_session_id:"root".into(),depth:0,cwd:"/".into() } }
    fn input() -> WorkpoolCreate { WorkpoolCreate { name:"pool".into(),agent:WorkpoolAgent::Category { prompt:"p".into(),model:None,category:"quick".into() },mode:Some(WorkpoolMode::Fresh),tools:None } }
    fn spec() -> WorkpoolSpec { WorkpoolSpec { start:ManagerStartSpec::default(),plan:ResolvedChildPlan::default() } }
    #[test] fn durable_create_load_list_and_revision() {
        let root=tempdir().expect("tempdir"); let s=WorkpoolStore::new(root.path()); let c=caller(); let r=s.create(&c,&input(),WorkpoolMode::Fresh,spec()).expect("create");
        let reopened=WorkpoolStore::new(root.path()); assert_eq!(reopened.load(&r.pool_id).expect("load").revision,0); assert_eq!(reopened.list().len(),1);
        let changed=reopened.mutate(&r.pool_id,|r|{let mut r=r.clone();r.status=super::super::types::WorkpoolStatus::Cancelled;r}).expect("mutate"); assert_eq!(changed.revision,1); assert_eq!(s.load(&r.pool_id).expect("persisted").revision,1);
    }
    #[test]
    fn stopped_records_keep_r0_base_and_round_trip_pause_metadata() {
        use super::super::types::*;
        let root = tempdir().expect("root"); let store = WorkpoolStore::new(root.path());
        let pool = store.create(&caller(), &input(), WorkpoolMode::Fresh, spec()).expect("create");
        let stopped = store.mutate(&pool.pool_id, |pool| {
            let mut next = pool.clone(); next.hold = Some(WorkpoolHold { reason: WorkpoolHoldReason::UserStop, pause_seq: 7 });
            next.workers.push(WorkpoolWorker { task_id: "st_worker".into(), run_epoch: 0, status: WorkpoolWorkerStatus::Paused, completed_turns: 0, idle_since: 0 });
            next.items.push(WorkpoolItem { key: "one".into(), input: 1.into(), item_id: "wi_one".into(), status: WorkpoolItemStatus::Assigned,
                binding: Some(WorkpoolBinding { task_id: "st_worker".into(), run_epoch: 0, generation: pool.generation }),
                delivery: Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Dispatching, message_sha256: None, entry_ids: Some(vec!["entry-1".into()]) }), data: None, error: None, yield_sha256: None }); next
        }).expect("stop");
        let loaded = WorkpoolStore::new(root.path()).load(&pool.pool_id).expect("reload");
        assert_eq!(loaded.hold, stopped.hold); assert_eq!(loaded.workers, stopped.workers); assert_eq!(loaded.items, stopped.items);
        let raw: Value = serde_json::from_slice(&fs::read(store.path(&pool.pool_id).expect("path")).expect("read")).expect("json");
        assert!(raw.get("hold").is_none()); assert_eq!(raw["workers"][0]["status"], "busy"); assert!(raw["items"][0]["delivery"].get("entry_ids").is_none());
        assert_eq!(store.list().len(), 1);
        for entry in fs::read_dir(store.directory()).expect("directory") {
            let path = entry.expect("entry").path();
            if path.file_name().expect("name").to_string_lossy().len() == 40 {
                let base: DiskRecord = serde_json::from_slice(&fs::read(path).expect("read")).expect("r0 base");
                assert!(base.hold.is_none()); assert!(base.workers.iter().all(|w| w.status != WorkpoolWorkerStatus::Paused));
            }
        }
        let base_path = store.path(&pool.pool_id).expect("path"); let old_base = fs::read(&base_path).expect("base");
        store.mutate(&pool.pool_id, |p| { let mut next=p.clone(); next.hold=None; next }).expect("release");
        // Simulate a crash after writing the sidecar but before replacing the base record.
        fs::write(&base_path, old_base).expect("rollback base");
        assert_eq!(store.load(&pool.pool_id).expect("previous revision").hold, stopped.hold);
        fs::write(base_path.with_extension("pause.json"), r#"{"revisions":{"1":{"hold":{"reason":"user_stop","pauseSeq":0},"paused_workers":[],"entry_ids":{}}}}"#).expect("corrupt sidecar");
        assert_eq!(store.load(&pool.pool_id).err().expect("invalid hold").code, WorkpoolErrorCode::StoreCorrupt);
    }
    #[test] fn missing_pool_is_typed() { let root=tempdir().expect("tempdir");let s=WorkpoolStore::new(root.path());assert_eq!(s.load("wp_00000000000000000000000000000000").err().expect("missing").code,WorkpoolErrorCode::PoolNotFound); }
    #[test] fn corrupt_record_is_typed() { let root=tempdir().expect("tempdir");let s=WorkpoolStore::new(root.path());let dir=root.path().join("workpools");fs::create_dir_all(&dir).expect("dir");fs::write(dir.join("wp_00000000000000000000000000000000.json"),"{").expect("write");assert_eq!(s.load("wp_00000000000000000000000000000000").err().expect("corrupt").code,WorkpoolErrorCode::StoreCorrupt); }
}
