use maho_core::{agent_session::AgentSession, sdk::{CreateAgentSessionOptions, create_agent_session}, session_manager::{CURRENT_SESSION_VERSION, NewSessionOptions, SessionManager, create_session_id, get_default_session_dir, load_entries_from_file, now_iso_millis, serialize_entry}};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, BTreeSet}, future::Future, pin::Pin, sync::Arc};
use tokio::sync::Mutex;
use indexmap::IndexMap;

pub type SessionFactory = Arc<dyn Fn(CreateAgentSessionOptions) -> Pin<Box<dyn Future<Output = Result<AgentSession, String>> + Send>> + Send + Sync>;

pub fn fork_session_manager(source_file: Option<&str>, cwd: &str, session_dir: Option<&str>) -> Result<SessionManager, String> {
    match source_file.filter(|path| std::path::Path::new(path).exists()) {
        Some(source_file) => fork_from_session_file(source_file, cwd, session_dir),
        None => Ok(SessionManager::create(cwd, session_dir, source_file.map(|path| NewSessionOptions { id: None, parent_session: Some(path.to_owned()) }))),
    }
}
pub fn fork_from_session_file(source_file: &str, cwd: &str, session_dir: Option<&str>) -> Result<SessionManager, String> {
    let entries = load_entries_from_file(source_file);
    if entries.is_empty() { return Err(format!("Cannot fork: source session file is empty or invalid: {source_file}")); }
    if !entries.iter().any(|entry| entry.get("type").and_then(Value::as_str) == Some("session")) { return Err(format!("Cannot fork: source session has no header: {source_file}")); }
    let dir = session_dir.map(str::to_owned).unwrap_or_else(|| get_default_session_dir(cwd));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let new_id = create_session_id();
    let timestamp = now_iso_millis();
    let file_timestamp = timestamp.replace([':', '.'], "-");
    let new_file = std::path::Path::new(&dir).join(format!("{file_timestamp}_{new_id}.jsonl"));
    let header = json!({"type":"session","version":CURRENT_SESSION_VERSION,"id":new_id,"timestamp":timestamp,"cwd":cwd,"parentSession":source_file});
    let mut contents = format!("{}\n", serde_json::to_string(&header).map_err(|error| error.to_string())?);
    for entry in &entries {
        if entry.get("type").and_then(Value::as_str) == Some("session") { continue; }
        contents.push_str(&serialize_entry(entry));
        contents.push('\n');
    }
    let mut file = std::fs::OpenOptions::new().create_new(true).write(true).open(&new_file).map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut file, contents.as_bytes()).map_err(|error| { drop(file); let _ = std::fs::remove_file(&new_file); error.to_string() })?;
    let path = new_file.to_string_lossy().into_owned();
    Ok(SessionManager::open(&path, session_dir, Some(cwd), None))
}

pub struct ThreadEntry {
    pub id: String,
    pub session: AgentSession,
    pub cwd: String,
    pub subscribers: BTreeSet<String>,
    pub active_turn: Option<String>,
    pub interrupted: bool,
    pub queued_terminal_notifications: Vec<Value>,
    pub created_at: String,
    pub updated_at: String,
    pub tasks: Arc<Mutex<()>>,
}
impl ThreadEntry {
    pub fn wire(&self) -> Value {
        json!({"id":self.id,"sessionId":self.session.session_id(),"sessionPath":self.session.session_file(),"cwd":self.cwd,"createdAt":self.created_at,"updatedAt":self.updated_at,"status":{"type":if self.active_turn.is_some() {"active"} else {"idle"}},"preview":self.session.get_user_messages_for_forking().first().map(|(_,text)|text),"name":self.session.session_name()})
    }
}

pub struct ThreadRegistry {
    entries: Mutex<IndexMap<String, Arc<Mutex<ThreadEntry>>>>,
    deleted: Mutex<BTreeSet<String>>,
    pub agent_dir: String,
    pub session_dir: Option<String>,
    factory: SessionFactory,
}
impl ThreadRegistry {
    pub fn new(agent_dir: String, session_dir: Option<String>, factory: Option<SessionFactory>) -> Self {
        Self { entries: Mutex::new(IndexMap::new()), deleted: Mutex::new(BTreeSet::new()), agent_dir, session_dir, factory: factory.unwrap_or_else(|| Arc::new(|options| Box::pin(async move { Ok(create_agent_session(options).await?.session) }))) }
    }
    pub async fn register_session(&self, session: AgentSession, cwd: String, timestamps: Option<(String, String)>) -> Arc<Mutex<ThreadEntry>> {
        let id = session.session_id();
        let mut entries = self.entries.lock().await;
        if let Some(existing) = entries.get(&id).cloned() {
            drop(entries);
            session.dispose().await;
            return existing;
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (created_at, updated_at) = timestamps.unwrap_or_else(|| (now.clone(), now));
        let entry = Arc::new(Mutex::new(ThreadEntry { id: id.clone(), session, cwd, subscribers: BTreeSet::new(), active_turn: None, interrupted: false, queued_terminal_notifications: Vec::new(), created_at, updated_at, tasks: Arc::new(Mutex::new(())) }));
        entries.insert(id, entry.clone());
        entry
    }
    pub async fn create_thread(&self, cwd: String, model: Option<maho_ai::model::Model>) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        let session = (self.factory)(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(self.agent_dir.clone()), session_manager: Some(SessionManager::create(&cwd, self.session_dir.as_deref(), None)), model, ..Default::default() }).await?;
        self.deleted.lock().await.remove(&session.session_id());
        Ok(self.register_session(session, cwd, None).await)
    }
    pub async fn fork_thread(&self, thread_id: &str, cwd: Option<String>) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        let source = self.resume_thread(thread_id).await?;
        let (source_cwd, source_file) = {let source = source.lock().await;(source.cwd.clone(),source.session.session_file())};
        let cwd = maho_core::paths::resolve_path(cwd.as_deref().unwrap_or(&source_cwd),&source_cwd,&Default::default());
        let manager = fork_session_manager(source_file.as_deref(),&cwd,self.session_dir.as_deref())?;
        let session = (self.factory)(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(self.agent_dir.clone()), session_manager: Some(manager), ..Default::default() }).await?;
        self.deleted.lock().await.remove(&session.session_id());
        Ok(self.register_session(session, cwd, None).await)
    }
    pub async fn get_loaded_thread(&self, id: &str) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        self.entries.lock().await.get(id).cloned().ok_or_else(|| format!("Thread not found: {id}"))
    }
    pub async fn resume_thread(&self, id: &str) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        if let Ok(entry) = self.get_loaded_thread(id).await { return Ok(entry); }
        if self.deleted.lock().await.contains(id) { return Err(format!("Thread not found: {id}")); }
        let info = self.list_session_infos().await.into_iter().find(|info| info.id == id).ok_or_else(|| format!("Thread not found: {id}"))?;
        let manager = SessionManager::open(&info.path, self.session_dir.as_deref(), Some(&info.cwd), None);
        let session = (self.factory)(CreateAgentSessionOptions { cwd: Some(info.cwd.clone()), agent_dir: Some(self.agent_dir.clone()), session_manager: Some(manager), ..Default::default() }).await?;
        let timestamps = (info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis, true), info.modified.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        Ok(self.register_session(session, info.cwd, Some(timestamps)).await)
    }
    pub async fn list_loaded(&self) -> Vec<Value> {
        let entries = self.entries.lock().await.values().cloned().collect::<Vec<_>>();
        let mut wires = Vec::new();
        for entry in entries { wires.push(entry.lock().await.wire()); }
        wires
    }
    pub async fn list_session_infos(&self) -> Vec<maho_core::session_discovery::SessionInfo> {
        let directories = if let Some(directory) = &self.session_dir {vec![directory.clone()]} else {
            let root = std::path::Path::new(&self.agent_dir).join("sessions");
            let mut directories = Vec::new();
            if let Ok(mut entries) = tokio::fs::read_dir(root).await {while let Ok(Some(entry)) = entries.next_entry().await {if entry.file_type().await.is_ok_and(|kind|kind.is_dir() || kind.is_symlink()) {directories.push(entry.path().display().to_string());}}}
            directories
        };
        let mut sessions = Vec::new();
        for directory in directories {sessions.extend(maho_core::session_discovery::list_sessions_from_dir(&directory,None,0,None));}
        sessions.sort_by_key(|info|std::cmp::Reverse(info.modified));
        sessions
    }
    pub async fn list_threads(&self,cursor: Option<&str>,limit: usize) -> Value {
        let offset = super::registry_listing::decode_cursor(cursor);
        let deleted = self.deleted.lock().await.clone();
        let mut threads = BTreeMap::new();
        for info in self.list_session_infos().await {if !deleted.contains(&info.id) {threads.insert(info.id.clone(),super::registry_listing::build_disk_thread(&info));}}
        for entry in self.list_loaded().await {if let Some(id) = entry["id"].as_str() {threads.insert(id.to_owned(),entry);}}
        let mut threads = threads.into_values().collect::<Vec<_>>();
        threads.sort_by(super::registry_listing::compare_threads);
        let page = threads.iter().skip(offset).take(limit).cloned().collect::<Vec<_>>();
        let next = offset.saturating_add(page.len());
        json!({"threads":page,"nextCursor":if next < threads.len() {Some(super::registry_listing::encode_cursor(next))} else {None}})
    }
    pub async fn remove_connection(&self, id: &str)->Vec<String> {
        let entries = self.entries.lock().await.values().cloned().collect::<Vec<_>>();
        let mut affected=Vec::new();
        for entry in entries {let mut entry=entry.lock().await;if entry.subscribers.remove(id) {affected.push(entry.id.clone());}}
        affected
    }
    pub async fn abort_active_turns(&self) {
        let entries = self.entries.lock().await.values().cloned().collect::<Vec<_>>();
        futures_util::future::join_all(entries.into_iter().map(|entry|async move {
            let session = {let mut entry = entry.lock().await; if entry.active_turn.is_some() {entry.interrupted = true;Some(entry.session.clone())} else {None}};
            if let Some(session) = session {session.abort().await;}
        })).await;
    }
    pub async fn unload_thread(&self, id: &str) -> bool {
        let entry = self.entries.lock().await.shift_remove(id);
        if let Some(entry) = entry { entry.lock().await.session.dispose().await; true } else { false }
    }
    pub async fn unload_if_idle(&self,id:&str)->bool {
        let mut entries=self.entries.lock().await;
        let Some(entry)=entries.get(id).cloned() else {return false;};
        let entry=entry.lock().await;
        if !entry.subscribers.is_empty()||entry.active_turn.is_some() {return false;}
        entries.shift_remove(id);drop(entries);
        entry.session.dispose().await;true
    }
    pub async fn delete_thread(&self,id: &str) -> std::io::Result<bool> {
        let loaded = self.entries.lock().await.shift_remove(id);
        let path = if let Some(entry) = loaded {
            let entry = entry.lock().await;
            entry.session.dispose().await;
            let path = entry.session.session_file();
            self.deleted.lock().await.insert(id.into());
            if let Some(path) = path {match tokio::fs::remove_file(path).await {Ok(())=>{},Err(error) if error.kind() == std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error)}}
            return Ok(true);
        } else {self.list_session_infos().await.into_iter().find(|info|info.id == id).map(|info|info.path)};
        self.deleted.lock().await.insert(id.into());
        if let Some(path) = path {match tokio::fs::remove_file(path).await {Ok(())=>{},Err(error) if error.kind() == std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error)}Ok(true)} else {Ok(false)}
    }
    pub async fn dispose(&self) {
        let entries = std::mem::take(&mut *self.entries.lock().await);
        for entry in entries.into_values() {
            let (tasks, session) = { let entry = entry.lock().await; (entry.tasks.clone(), entry.session.clone()) };
            let _guard = tasks.lock().await;
            session.dispose().await;
        }
    }
}
