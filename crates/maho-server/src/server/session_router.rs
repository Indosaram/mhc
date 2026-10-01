use super::{
    errors::ServerError,
    types::{Context, Publisher, RoutedSessionAttachment, RoutedSessionHandle, ServerHost},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub struct Attachment {
    pub target: Value,
    pub lease: Arc<dyn RoutedSessionAttachment>,
    operations: RwLock<()>,
    released: AtomicBool,
}
impl Attachment {
    pub async fn invoke(
        &self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> Result<Option<Value>, ServerError> {
        let _operation = self.operations.read().await;
        if self.released.load(Ordering::SeqCst) {
            return Err(ServerError::not_attached());
        }
        self.lease.invoke_service(call, publish, context).await
    }
    pub async fn release(&self) -> Result<(), ServerError> {
        let _operations = self.operations.write().await;
        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.lease.release().await
    }
}

pub struct SessionRouter {
    host: Arc<dyn ServerHost>,
    server_id: String,
    hosted: Mutex<BTreeMap<String, Arc<dyn RoutedSessionHandle>>>,
    attachments: Mutex<BTreeMap<String, Vec<Weak<Attachment>>>>,
    closing: AtomicBool,
}
impl SessionRouter {
    pub fn new(host: Arc<dyn ServerHost>, server_id: String) -> Self {
        Self {
            host,
            server_id,
            hosted: Mutex::new(BTreeMap::new()),
            attachments: Mutex::new(BTreeMap::new()),
            closing: AtomicBool::new(false),
        }
    }
    pub async fn attach(&self, session_id: &str) -> Result<Arc<Attachment>, ServerError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(ServerError::draining());
        }
        let handle = {
            let mut hosted = self.hosted.lock().await;
            if let Some(handle) = hosted.get(session_id) {
                handle.clone()
            } else {
                let metadata = self.host.resolve_session(session_id).await?;
                let handle = self.host.open_session(metadata).await?;
                if self.closing.load(Ordering::SeqCst) {
                    handle.close().await?;
                    return Err(ServerError::draining());
                }
                hosted.insert(session_id.into(), handle.clone());
                handle
            }
        };
        let lease = handle.attach_client().await?;
        if self.closing.load(Ordering::SeqCst) {
            lease.release().await?;
            return Err(ServerError::draining());
        }
        let attachment = Arc::new(Attachment {
            target: json!({"serverId":self.server_id,"sessionId":session_id,"attachmentId":uuid::Uuid::new_v4().to_string()}),
            lease,
            operations: RwLock::new(()),
            released: AtomicBool::new(false),
        });
        let mut attachments = self.attachments.lock().await;
        if self.closing.load(Ordering::SeqCst) || !self.hosted.lock().await.contains_key(session_id) {
            attachment.release().await?;
            return Err(ServerError::draining());
        }
        let leases = attachments.entry(session_id.into()).or_default();
        leases.retain(|lease| lease.strong_count() > 0);
        leases.push(Arc::downgrade(&attachment));
        Ok(attachment)
    }
    pub async fn remove(&self, session_id: &str) -> Result<(), ServerError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(ServerError::draining());
        }
        let (leases, handle) = {
            let mut attachments = self.attachments.lock().await;
            let handle = self.hosted.lock().await.remove(session_id);
            (attachments.remove(session_id).unwrap_or_default(), handle)
        };
        let mut errors = Vec::new();
        for lease in leases.into_iter().filter_map(|lease| lease.upgrade()) {
            if let Err(error) = lease.release().await { errors.push(error.message); }
        }
        if let Some(handle) = handle && let Err(error) = handle.close().await {
            errors.push(error.message);
        }
        if !errors.is_empty() {
            return Err(ServerError::new("internal_error", &errors.join("; ")));
        }
        Ok(())
    }
    pub async fn close(&self) -> Result<(), ServerError> {
        self.closing.store(true, Ordering::SeqCst);
        let attachments = std::mem::take(&mut *self.attachments.lock().await);
        let handles = std::mem::take(&mut *self.hosted.lock().await);
        let mut errors = Vec::new();
        for lease in attachments.into_values().flatten().filter_map(|lease| lease.upgrade()) {
            if let Err(error) = lease.release().await { errors.push(error.message); }
        }
        for (_, handle) in handles {
            if let Err(error) = handle.close().await {
                errors.push(error.message);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServerError::new("internal_error", &errors.join("; ")))
        }
    }
}
