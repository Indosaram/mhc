use super::{
    errors::ServerError,
    types::{Context, Publisher, RoutedSessionAttachment, RoutedSessionHandle, ServerHost},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub struct Attachment {
    pub target: Value,
    pub lease: Arc<dyn RoutedSessionAttachment>,
    operations: RwLock<()>,
}
impl Attachment {
    pub async fn invoke(
        &self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> Result<Option<Value>, ServerError> {
        let _operation = self.operations.read().await;
        self.lease.invoke_service(call, publish, context).await
    }
    pub async fn release(&self) -> Result<(), ServerError> {
        let _operations = self.operations.write().await;
        self.lease.release().await
    }
}

pub struct SessionRouter {
    host: Arc<dyn ServerHost>,
    server_id: String,
    hosted: Mutex<BTreeMap<String, Arc<dyn RoutedSessionHandle>>>,
    closing: AtomicBool,
}
impl SessionRouter {
    pub fn new(host: Arc<dyn ServerHost>, server_id: String) -> Self {
        Self {
            host,
            server_id,
            hosted: Mutex::new(BTreeMap::new()),
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
        Ok(Arc::new(Attachment {
            target: json!({"serverId":self.server_id,"sessionId":session_id,"attachmentId":uuid::Uuid::new_v4().to_string()}),
            lease,
            operations: RwLock::new(()),
        }))
    }
    pub async fn remove(&self, session_id: &str) -> Result<(), ServerError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(ServerError::draining());
        }
        if let Some(handle) = self.hosted.lock().await.remove(session_id) {
            handle.close().await?;
        }
        Ok(())
    }
    pub async fn close(&self) -> Result<(), ServerError> {
        self.closing.store(true, Ordering::SeqCst);
        let handles = std::mem::take(&mut *self.hosted.lock().await);
        let mut errors = Vec::new();
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
