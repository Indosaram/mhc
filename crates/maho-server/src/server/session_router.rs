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
use tokio::sync::{Mutex, RwLock, watch};

pub struct Attachment {
    pub target: Value,
    pub lease: Arc<dyn RoutedSessionAttachment>,
    client_lock: Mutex<()>,
    released: AtomicBool,
    release_result: tokio::sync::OnceCell<Result<(),ServerError>>,
}
impl Attachment {
    pub async fn invoke(
        &self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> Result<Option<Value>, ServerError> {
        let _client = self.client_lock.lock().await;
        if self.released.load(Ordering::SeqCst) {
            return Err(ServerError::not_attached());
        }
        self.lease.invoke_service(call, publish, context).await
    }
    pub async fn release(&self) -> Result<(), ServerError> {
        self.release_result.get_or_init(||async {
            let _client = self.client_lock.lock().await;
            self.released.store(true, Ordering::SeqCst);
            self.lease.release().await
        }).await.clone()
    }
}

pub struct SessionRouter {
    host: Arc<dyn ServerHost>,
    server_id: String,
    hosted: Mutex<BTreeMap<String, Arc<dyn RoutedSessionHandle>>>,
    opening: Mutex<BTreeMap<String, Weak<OpenSlot>>>,
    admissions: RwLock<()>,
    attachments: Mutex<BTreeMap<String, Vec<Weak<Attachment>>>>,
    closing: AtomicBool,
    close_result: tokio::sync::OnceCell<Result<(),ServerError>>,
    watched: Mutex<BTreeMap<String, Arc<dyn RoutedSessionHandle>>>,
}
struct OpenSlot { result: watch::Sender<Option<Result<Arc<dyn RoutedSessionHandle>, ServerError>>> }
enum Acquire { Wait(Arc<OpenSlot>), Lead(Arc<OpenSlot>) }
impl SessionRouter {
    pub fn new(host: Arc<dyn ServerHost>, server_id: String) -> Self {
        Self {
            host,
            server_id,
            hosted: Mutex::new(BTreeMap::new()),
            opening: Mutex::new(BTreeMap::new()),
            admissions: RwLock::new(()),
            attachments: Mutex::new(BTreeMap::new()),
            closing: AtomicBool::new(false),
            close_result: tokio::sync::OnceCell::new(),
            watched: Mutex::new(BTreeMap::new()),
        }
    }
    pub async fn attach(&self, session_id: &str) -> Result<Arc<Attachment>, ServerError> {
        let _admission=self.admissions.read().await;
        if self.closing.load(Ordering::SeqCst) {
            return Err(ServerError::draining());
        }
        let handle = self.acquire(session_id).await?;
        let lease = handle.attach_client().await?;
        if self.closing.load(Ordering::SeqCst) {
            lease.release().await?;
            return Err(ServerError::draining());
        }
        let attachment = Arc::new(Attachment {
            target: json!({"serverId":self.server_id,"sessionId":session_id,"attachmentId":uuid::Uuid::new_v4().to_string()}),
            lease,
            client_lock: Mutex::new(()),
            released: AtomicBool::new(false),
            release_result: tokio::sync::OnceCell::new(),
        });
        let mut attachments = self.attachments.lock().await;
        if self.closing.load(Ordering::SeqCst) || !self.hosted.lock().await.get(session_id).is_some_and(|current|Arc::ptr_eq(current,&handle)) {
            attachment.release().await?;
            return Err(ServerError::draining());
        }
        let leases = attachments.entry(session_id.into()).or_default();
        leases.retain(|lease| lease.strong_count() > 0);
        leases.push(Arc::downgrade(&attachment));
        Ok(attachment)
    }
    async fn acquire(&self, session_id: &str) -> Result<Arc<dyn RoutedSessionHandle>, ServerError> {
        loop {
            if let Some(handle) = self.hosted.lock().await.get(session_id).cloned() {
                return Ok(handle);
            }
            let decision = {
                let mut opening = self.opening.lock().await;
                opening.retain(|_, slot| slot.strong_count() > 0);
                match opening.get(session_id).and_then(Weak::upgrade) {
                    Some(slot) => Acquire::Wait(slot),
                    None => {
                        let slot = Arc::new(OpenSlot { result: watch::channel(None).0 });
                        opening.insert(session_id.into(), Arc::downgrade(&slot));
                        Acquire::Lead(slot)
                    }
                }
            };
            match decision {
                Acquire::Wait(slot) => {
                    let mut receiver = slot.result.subscribe();
                    loop {
                        if let Some(result) = receiver.borrow_and_update().clone() { return result; }
                        if receiver.changed().await.is_err() { break; }
                    }
                    let mut opening = self.opening.lock().await;
                    if opening.get(session_id).and_then(Weak::upgrade).is_some_and(|current| Arc::ptr_eq(&current, &slot)) {
                        opening.remove(session_id);
                    }
                }
                Acquire::Lead(slot) => {
                    let result = self.open(session_id).await;
                    slot.result.send_replace(Some(result.clone()));
                    let mut opening = self.opening.lock().await;
                    if opening.get(session_id).and_then(Weak::upgrade).is_some_and(|current| Arc::ptr_eq(&current, &slot)) {
                        opening.remove(session_id);
                    }
                    return result;
                }
            }
        }
    }
    async fn open(&self, session_id: &str) -> Result<Arc<dyn RoutedSessionHandle>, ServerError> {
        let metadata = self.host.resolve_session(session_id).await?;
        let handle = self.host.open_session(metadata).await?;
        if self.closing.load(Ordering::SeqCst) {
            handle.close().await?;
            return Err(ServerError::draining());
        }
        self.hosted.lock().await.insert(session_id.into(), handle.clone());
        Ok(handle)
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
        self.close_result.get_or_init(||self.close_internal()).await.clone()
    }
    pub async fn watch_termination(self: &Arc<Self>, session_id: &str) {
        let handle = self.hosted.lock().await.get(session_id).cloned();
        let Some(handle) = handle else { return };
        let Some(terminated) = handle.terminated() else { return };
        {
            let mut watched = self.watched.lock().await;
            if watched.get(session_id).is_some_and(|current| Arc::ptr_eq(current, &handle)) { return; }
            watched.insert(session_id.to_owned(), handle.clone());
        }
        let router = Arc::clone(self);
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            let error = terminated.await;
            router.invalidate(&session_id, &handle, error).await;
        });
    }
    async fn invalidate(&self, session_id: &str, handle: &Arc<dyn RoutedSessionHandle>, error: Option<ServerError>) {
        let leases = {
            let mut hosted = self.hosted.lock().await;
            if !hosted.get(session_id).is_some_and(|current| Arc::ptr_eq(current, handle)) { return; }
            hosted.remove(session_id);
            let mut watched = self.watched.lock().await;
            if watched.get(session_id).is_some_and(|current| Arc::ptr_eq(current, handle)) { watched.remove(session_id); }
            drop(watched);
            self.attachments.lock().await.remove(session_id).unwrap_or_default()
        };
        for lease in leases.into_iter().filter_map(|lease| lease.upgrade()) {
            if let Err(release) = lease.release().await { eprintln!("{}", release.message); }
        }
        if let Err(close) = handle.close().await { eprintln!("{}", close.message); }
        if let Some(error) = error { eprintln!("{}", error.message); }
    }
    async fn close_internal(&self) -> Result<(), ServerError> {
        let _admissions=self.admissions.write().await;
        let attachments = std::mem::take(&mut *self.attachments.lock().await);
        let handles = std::mem::take(&mut *self.hosted.lock().await);
        let mut errors = Vec::new();
        let leases=attachments.into_values().flatten().filter_map(|lease|lease.upgrade()).collect::<Vec<_>>();
        for result in futures_util::future::join_all(leases.iter().map(|lease|lease.release())).await {
            if let Err(error) = result { errors.push(error.message); }
        }
        for result in futures_util::future::join_all(handles.values().map(|handle|handle.close())).await {
            if let Err(error) = result {
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
