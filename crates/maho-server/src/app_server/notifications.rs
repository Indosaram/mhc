use super::{envelope::populate_outbound_notification, methods::EXPERIMENTAL_SERVER_NOTIFICATION_METHODS, registry::JsonRpcError};
use serde_json::Value;
use std::{collections::{BTreeMap, BTreeSet, VecDeque}, future::Future, pin::Pin, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}};

pub type SendFuture = Pin<Box<dyn Future<Output = Result<(), JsonRpcError>> + Send>>;
pub type SendMessage = Arc<dyn Fn(Value) -> SendFuture + Send + Sync>;
pub struct RoutableConnection {
    pub id: String,
    pub initialized: bool,
    pub stdio: bool,
    pub experimental_api: bool,
    pub opt_out_notification_methods: BTreeSet<String>,
    pub send: SendMessage,
    pub close: Option<Arc<dyn Fn() + Send + Sync>>,
}
struct ConnectionState {
    connection: RoutableConnection,
    pending: Arc<AtomicUsize>,
    closed: AtomicBool,
}
#[derive(Default)]
pub struct RoutableThread {
    pub subscribers: BTreeSet<String>,
    pub queued_terminal_notifications: VecDeque<Value>,
}
pub struct NotificationRouter {
    connections: BTreeMap<String, ConnectionState>,
    threads: BTreeMap<String, RoutableThread>,
    pub outbound_queue_limit: usize,
    pub terminal_queue_limit: usize,
    pub experimental_notification_methods: BTreeSet<String>,
}
impl Default for NotificationRouter {
    fn default() -> Self {
        Self { connections: BTreeMap::new(), threads: BTreeMap::new(), outbound_queue_limit: 32_768, terminal_queue_limit: 100, experimental_notification_methods: EXPERIMENTAL_SERVER_NOTIFICATION_METHODS.iter().map(|method|(*method).to_owned()).collect() }
    }
}
impl NotificationRouter {
    pub fn set_experimental_notification_methods(&mut self,methods: impl IntoIterator<Item=String>) {self.experimental_notification_methods = methods.into_iter().collect();}
    pub fn add_connection(&mut self, connection: RoutableConnection) {
        self.connections.insert(connection.id.clone(), ConnectionState { connection, pending: Arc::new(AtomicUsize::new(0)), closed: AtomicBool::new(false) });
    }
    pub fn remove_connection(&mut self, id: &str) -> Vec<String> {
        self.connections.remove(id);
        self.threads.iter_mut().filter_map(|(thread_id, thread)| {
            (thread.subscribers.remove(id) && thread.subscribers.is_empty()).then(|| thread_id.clone())
        }).collect()
    }
    pub fn add_thread(&mut self, id: String, thread: RoutableThread) { self.threads.insert(id, thread); }
    pub fn remove_thread(&mut self, id: &str) { self.threads.remove(id); }
    pub fn subscribe(&mut self, thread_id: &str, connection_id: &str) {
        let Some(thread) = self.threads.get_mut(thread_id) else { return; };
        thread.subscribers.insert(connection_id.to_owned());
        let Some(connection) = self.connections.get(connection_id) else { return; };
        for message in thread.queued_terminal_notifications.drain(..) { Self::enqueue(connection, message, self.outbound_queue_limit, &self.experimental_notification_methods); }
    }
    pub fn unsubscribe(&mut self, thread_id: &str, connection_id: &str) {
        if let Some(thread) = self.threads.get_mut(thread_id) { thread.subscribers.remove(connection_id); }
    }
    pub fn broadcast(&self, notification: Value, now: u64) -> Result<(), JsonRpcError> {
        let method = notification["method"].as_str().unwrap_or_default();
        if !["remoteControl/status/changed", "thread/started", "thread/status/changed", "thread/closed", "thread/deleted", "thread/archived", "thread/unarchived", "thread/name/updated", "thread/goal/updated", "thread/goal/cleared", "thread/tokenUsage/updated", "fuzzyFileSearch/sessionUpdated", "fuzzyFileSearch/sessionCompleted"].contains(&method) {
            return Err(JsonRpcError::new(-32600, format!("Notification method {method} is not allowed for broadcast")));
        }
        let message = populate_outbound_notification(notification, now);
        for connection in self.connections.values().filter(|state| state.connection.initialized) { Self::enqueue(connection, message.clone(), self.outbound_queue_limit, &self.experimental_notification_methods); }
        Ok(())
    }
    pub fn to_thread(&mut self, thread_id: &str, message: Value, now: u64) {
        let Some(thread) = self.threads.get_mut(thread_id) else { return; };
        let message = populate_outbound_notification(message, now);
        let mut routed = 0;
        for id in &thread.subscribers {
            if let Some(connection) = self.connections.get(id).filter(|state| state.connection.initialized) {
                routed += 1;
                Self::enqueue(connection, message.clone(), self.outbound_queue_limit, &self.experimental_notification_methods);
            }
        }
        if routed == 0 && message.get("id").is_none() && matches!(message["method"].as_str(), Some("turn/completed" | "error")) {
            thread.queued_terminal_notifications.push_back(message);
            while thread.queued_terminal_notifications.len() > self.terminal_queue_limit { thread.queued_terminal_notifications.pop_front(); }
        }
    }
    fn enqueue(state: &ConnectionState, message: Value, limit: usize, experimental: &BTreeSet<String>) {
        let connection = &state.connection;
        let method = message["method"].as_str().unwrap_or_default();
        if state.closed.load(Ordering::SeqCst) || (message.get("id").is_none() && (connection.opt_out_notification_methods.contains(method) || (experimental.contains(method) && !connection.experimental_api))) { return; }
        if !connection.stdio && state.pending.load(Ordering::SeqCst) >= limit {
            state.closed.store(true, Ordering::SeqCst);
            if let Some(close) = &connection.close { close(); }
            return;
        }
        state.pending.fetch_add(1, Ordering::SeqCst);
        let result = (connection.send)(message);
        let pending = state.pending.clone();
        tokio::spawn(async move {
            let _result = result.await;
            pending.fetch_sub(1, Ordering::SeqCst);
        });
    }
}
