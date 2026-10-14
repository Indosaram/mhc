use maho_core::{agent_session::AgentSession, sdk::{CreateAgentSessionOptions, create_agent_session}, session_manager::{CURRENT_SESSION_VERSION, NewSessionOptions, SessionManager, create_session_id, get_default_session_dir, load_entries_from_file, now_iso_millis, serialize_entry}};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, BTreeSet}, future::Future, pin::Pin, sync::Arc};
use tokio::sync::Mutex;
use indexmap::IndexMap;

pub type SessionFactory = Arc<dyn Fn(CreateAgentSessionOptions) -> Pin<Box<dyn Future<Output = Result<AgentSession, String>> + Send>> + Send + Sync>;
/// senpi `createAppServerUIContext`: the thread's extension UI, handed to the session factory so it
/// is bound before `session_start`.
pub type SessionUiFactory = Arc<dyn Fn(&str) -> Option<Arc<dyn maho_ext_api::ExtensionUi>> + Send + Sync>;
/// The rest of senpi `createBoundAppServerSession`: binds the UI and the MCP inventory of every newly
/// registered session, whichever handler loaded it.
pub type SessionBinder = Arc<dyn Fn(String, AgentSession, Option<Arc<dyn maho_ext_api::ExtensionUi>>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// senpi `ThreadRegistry` default `createSession`: the SDK `createAgentSession`.
pub fn default_session_factory() -> SessionFactory {
    Arc::new(|options| Box::pin(async move { Ok(create_agent_session(options).await?.session) }))
}
/// senpi `AppServerSessionResult.initialNotifications` cap (the router terminal queue limit).
const INITIAL_NOTIFICATION_LIMIT: usize = 100;

pub fn fork_session_manager(source_file: Option<&str>, cwd: &str, session_dir: Option<&str>) -> Result<SessionManager, String> {
    match source_file.filter(|path| std::path::Path::new(path).exists()) {
        Some(source_file) => fork_from_session_file(source_file, cwd, session_dir),
        None => Ok(SessionManager::create(cwd, session_dir, source_file.map(|path| NewSessionOptions { id: None, parent_session: Some(path.to_owned()) }))),
    }
}
pub fn fork_from_session_file(source_file: &str, cwd: &str, session_dir: Option<&str>) -> Result<SessionManager, String> {
    let entries = load_entries_from_file(source_file);
    if entries.is_empty() {
        let has_content = std::fs::read_to_string(source_file).map(|contents| contents.lines().any(|line| !line.trim().is_empty())).unwrap_or(false);
        let reason = if has_content { "no header" } else { "empty or invalid" };
        return Err(format!("Cannot fork: source session {reason}: {source_file}"));
    }
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
    /// senpi `pendingByThreadId`: the open turn awaiting `completeTurn`.
    pub pending_turn: Option<super::turns::PendingTurn>,
    /// senpi `projectorByThreadId`: the active turn's projector, keyed by its turn id.
    pub projection: Option<(String, super::projection::EventProjector)>,
    /// senpi `subscribedThreadIds`: the one session subscription feeding the turn engine.
    pub observer: Option<maho_core::agent_session::AgentSessionSubscription>,
    /// Ordered input of the observer task: session events plus completions queued behind them.
    pub observer_events: Option<tokio::sync::mpsc::UnboundedSender<super::turns::ObservedEvent>>,
    /// Held by `turn/start` until its initial notifications are out, so projected session events
    /// never reach the wire before `turn/started`; the observer takes it per event.
    pub emit_gate: Arc<Mutex<()>>,
    /// Synchronous mirror of `active_turn` for senpi `getTurnId` (`activeTurn?.turnId`).
    active_turn_ids: ActiveTurnIds,
}
/// Thread id -> active turn id, readable from the synchronous extension UI context.
pub type ActiveTurnIds = Arc<std::sync::Mutex<BTreeMap<String, String>>>;
/// senpi runtime `getTurnId`: the thread's active turn id, or `"turn-user-input"`.
pub fn active_turn_id_or_default(active: &ActiveTurnIds, thread_id: &str) -> String {
    active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(thread_id).cloned().unwrap_or_else(|| "turn-user-input".to_owned())
}
impl ThreadEntry {
    /// Set `active_turn` and its synchronous mirror together.
    pub fn set_active_turn(&mut self, turn_id: Option<String>) {
        let mut active = self.active_turn_ids.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &turn_id { Some(turn_id) => { active.insert(self.id.clone(), turn_id.clone()); }, None => { active.remove(&self.id); } }
        drop(active);
        self.active_turn = turn_id;
    }
    pub fn wire(&self) -> Value {
        json!({"id":self.id,"sessionId":self.session.session_id(),"sessionPath":self.session.session_file(),"cwd":self.cwd,"createdAt":self.created_at,"updatedAt":self.updated_at,"status":{"type":if self.active_turn.is_some() {"active"} else {"idle"}},"preview":self.session.get_user_messages_for_forking().first().map(|(_,text)|text),"name":self.session.session_name()})
    }
}

pub struct ThreadRegistry {
    entries: Mutex<IndexMap<String, Arc<Mutex<ThreadEntry>>>>,
    deleted: Mutex<BTreeSet<String>>,
    /// Notifications a session produced before its thread entry exists (senpi `initialNotifications`).
    initial_notifications: std::sync::Mutex<BTreeMap<String, Vec<Value>>>,
    pub agent_dir: String,
    pub session_dir: Option<String>,
    factory: SessionFactory,
    binding: std::sync::OnceLock<(SessionUiFactory, SessionBinder)>,
    active_turn_ids: ActiveTurnIds,
}
impl ThreadRegistry {
    pub fn new(agent_dir: String, session_dir: Option<String>, factory: Option<SessionFactory>) -> Self {
        Self { entries: Mutex::new(IndexMap::new()), deleted: Mutex::new(BTreeSet::new()), initial_notifications: std::sync::Mutex::new(BTreeMap::new()), agent_dir, session_dir, factory: factory.unwrap_or_else(default_session_factory), binding: std::sync::OnceLock::new(), active_turn_ids: ActiveTurnIds::default() }
    }
    /// Queue notifications for a session whose thread entry is registered next (marking it as
    /// awaiting registration even when empty); they become the
    /// entry's queued terminal notifications, replayed to its first subscriber.
    pub fn queue_initial_notifications(&self, id: &str, notifications: impl IntoIterator<Item = Value>) {
        let mut pending = self.initial_notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let queue = pending.entry(id.to_owned()).or_default();
        queue.extend(notifications);
        let excess = queue.len().saturating_sub(INITIAL_NOTIFICATION_LIMIT);
        queue.drain(..excess);
    }
    /// The loaded entry for `id`; while a created session awaits registration its notification is
    /// queued as an initial notification instead, and for any other thread it is dropped (senpi
    /// `toThread` on an unknown thread). Checked under the entries lock so registration cannot race.
    pub async fn loaded_or_queue_initial(&self, id: &str, notification: Value) -> Option<Arc<Mutex<ThreadEntry>>> {
        let entries = self.entries.lock().await;
        if let Some(entry) = entries.get(id) { return Some(entry.clone()); }
        let awaiting = self.initial_notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(id);
        if awaiting { self.queue_initial_notifications(id, [notification]); }
        None
    }
    /// The shared active-turn mirror behind senpi `getTurnId`.
    pub fn active_turn_ids(&self) -> ActiveTurnIds { self.active_turn_ids.clone() }
    /// Install the app-server session binding used for every session this registry loads.
    pub fn bind_sessions(&self, ui: SessionUiFactory, binder: SessionBinder) {
        if self.binding.set((ui, binder)).is_err() { eprintln!("app-server session binding is installed once"); }
    }
    fn session_ui(&self, id: &str) -> Option<Arc<dyn maho_ext_api::ExtensionUi>> {
        self.binding.get().and_then(|(ui, _)| ui(id))
    }
    pub async fn register_session(&self, session: AgentSession, cwd: String, timestamps: Option<(String, String)>) -> Arc<Mutex<ThreadEntry>> {
        let ui = self.session_ui(&session.session_id());
        self.register_bound_session(session, cwd, timestamps, ui).await
    }
    async fn register_bound_session(&self, session: AgentSession, cwd: String, timestamps: Option<(String, String)>, ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>) -> Arc<Mutex<ThreadEntry>> {
        let id = session.session_id();
        let mut entries = self.entries.lock().await;
        let initial = self.initial_notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id).unwrap_or_default();
        if let Some(existing) = entries.get(&id).cloned() {
            drop(entries);
            session.dispose().await;
            return existing;
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (created_at, updated_at) = timestamps.unwrap_or_else(|| (now.clone(), now));
        let bound = session.clone();
        let entry = Arc::new(Mutex::new(ThreadEntry { id: id.clone(), session, cwd, subscribers: BTreeSet::new(), active_turn: None, interrupted: false, queued_terminal_notifications: initial, created_at, updated_at, tasks: Arc::new(Mutex::new(())), pending_turn: None, projection: None, observer: None, observer_events: None, emit_gate: Arc::new(Mutex::new(())), active_turn_ids: self.active_turn_ids.clone() }));
        entries.insert(id.clone(), entry.clone());
        drop(entries);
        if let Some((_, binder)) = self.binding.get() { binder(id, bound, ui).await; }
        entry
    }
    pub async fn create_thread(&self, cwd: String, model: Option<maho_ai::model::Model>) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        let manager = SessionManager::create(&cwd, self.session_dir.as_deref(), None);
        if let Some(path) = manager.session_file()
            && !std::path::Path::new(path).exists()
            && let Some(header) = manager.header() {
            std::fs::write(path, format!("{}\n", serialize_entry(&header))).map_err(|error| error.to_string())?;
        }
        let ui = self.session_ui(manager.session_id());
        let session = (self.factory)(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(self.agent_dir.clone()), session_manager: Some(manager), model, extension_ui: ui.clone(), ..Default::default() }).await?;
        self.deleted.lock().await.remove(&session.session_id());
        Ok(self.register_bound_session(session, cwd, None, ui).await)
    }
    pub async fn fork_thread(&self, thread_id: &str, cwd: Option<String>) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        let source = self.resume_thread(thread_id).await?;
        let (source_cwd, source_file) = {let source = source.lock().await;(source.cwd.clone(),source.session.session_file())};
        let cwd = maho_core::paths::resolve_path(cwd.as_deref().unwrap_or(&source_cwd),&source_cwd,&Default::default());
        let manager = fork_session_manager(source_file.as_deref(),&cwd,self.session_dir.as_deref())?;
        let ui = self.session_ui(manager.session_id());
        let session = (self.factory)(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(self.agent_dir.clone()), session_manager: Some(manager), extension_ui: ui.clone(), ..Default::default() }).await?;
        self.deleted.lock().await.remove(&session.session_id());
        Ok(self.register_bound_session(session, cwd, None, ui).await)
    }
    pub async fn get_loaded_thread(&self, id: &str) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        self.entries.lock().await.get(id).cloned().ok_or_else(|| format!("Thread not found: {id}"))
    }
    pub async fn resume_thread(&self, id: &str) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        if let Ok(entry) = self.get_loaded_thread(id).await { return Ok(entry); }
        if self.deleted.lock().await.contains(id) { return Err(format!("Thread not found: {id}")); }
        let info = self.list_session_infos().await.into_iter().find(|info| info.id == id).ok_or_else(|| format!("Thread not found: {id}"))?;
        let manager = SessionManager::open(&info.path, self.session_dir.as_deref(), Some(&info.cwd), None);
        let ui = self.session_ui(manager.session_id());
        let session = (self.factory)(CreateAgentSessionOptions { cwd: Some(info.cwd.clone()), agent_dir: Some(self.agent_dir.clone()), session_manager: Some(manager), extension_ui: ui.clone(), ..Default::default() }).await?;
        let timestamps = (info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis, true), info.modified.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        Ok(self.register_bound_session(session, info.cwd, Some(timestamps), ui).await)
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
