use maho_core::{agent_session::AgentSession, sdk::{CreateAgentSessionOptions, create_agent_session}, session_manager::SessionManager};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, BTreeSet}, future::Future, pin::Pin, sync::Arc};
use tokio::sync::Mutex;

pub type SessionFactory = Arc<dyn Fn(CreateAgentSessionOptions) -> Pin<Box<dyn Future<Output = Result<AgentSession, String>> + Send>> + Send + Sync>;

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
    entries: Mutex<BTreeMap<String, Arc<Mutex<ThreadEntry>>>>,
    deleted: Mutex<BTreeSet<String>>,
    pub agent_dir: String,
    pub session_dir: Option<String>,
    factory: SessionFactory,
}
impl ThreadRegistry {
    pub fn new(agent_dir: String, session_dir: Option<String>, factory: Option<SessionFactory>) -> Self {
        Self { entries: Mutex::new(BTreeMap::new()), deleted: Mutex::new(BTreeSet::new()), agent_dir, session_dir, factory: factory.unwrap_or_else(|| Arc::new(|options| Box::pin(async move { Ok(create_agent_session(options).await?.session) }))) }
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
    pub async fn get_loaded_thread(&self, id: &str) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        self.entries.lock().await.get(id).cloned().ok_or_else(|| format!("Thread not found: {id}"))
    }
    pub async fn resume_thread(&self, id: &str) -> Result<Arc<Mutex<ThreadEntry>>, String> {
        if let Ok(entry) = self.get_loaded_thread(id).await { return Ok(entry); }
        if self.deleted.lock().await.contains(id) { return Err(format!("Thread not found: {id}")); }
        let directory = self.session_dir.as_deref().ok_or_else(|| format!("Thread not found: {id}"))?;
        let info = maho_core::session_discovery::list_sessions_from_dir(directory, None, 0, None).into_iter().find(|info| info.id == id).ok_or_else(|| format!("Thread not found: {id}"))?;
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
    pub async fn remove_connection(&self, id: &str) {
        let entries = self.entries.lock().await.values().cloned().collect::<Vec<_>>();
        for entry in entries { entry.lock().await.subscribers.remove(id); }
    }
    pub async fn abort_active_turns(&self) {
        let entries = self.entries.lock().await.values().cloned().collect::<Vec<_>>();
        for entry in entries {
            let session = {let mut entry = entry.lock().await; if entry.active_turn.is_some() {entry.interrupted = true;Some(entry.session.clone())} else {None}};
            if let Some(session) = session {session.abort().await;}
        }
    }
    pub async fn unload_thread(&self, id: &str) -> bool {
        let entry = self.entries.lock().await.remove(id);
        if let Some(entry) = entry { entry.lock().await.session.dispose().await; true } else { false }
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
