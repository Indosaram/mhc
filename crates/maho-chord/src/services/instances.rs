use crate::{context::{BACKGROUND_CONTEXT, with_cancel, CancelHandle}, types::*};
use super::{errors::contract, state::{lock, report_error}};
use futures::FutureExt;
use std::{collections::BTreeMap, sync::{Arc, Mutex}};

pub trait InstanceDirectoryEntry: Send + Sync {
    fn key(&self) -> &JsString;
    fn generation(&self) -> ServiceSequence;
    fn service(&self) -> ServiceImplementation;
    fn deactivate(&self);
}
struct Observer { handler: ServiceObserver, tasks: BTreeMap<JsString, CancelHandle> }
struct Directory<T> { entries: BTreeMap<JsString, Arc<T>>, observers: Vec<(Arc<()>, Observer)>, ready: bool, disposed: bool }
pub struct InstanceDirectory<T> { data: Mutex<Directory<T>>, report: ErrorHandler }
impl<T: InstanceDirectoryEntry + 'static> InstanceDirectory<T> {
    pub fn new(ready: bool, on_error: ErrorHandler) -> Arc<Self> { Arc::new(Self { data: Mutex::new(Directory { entries: BTreeMap::new(), observers: Vec::new(), ready, disposed: false }), report: on_error }) }
    pub fn observer_count(&self) -> usize { lock(&self.data).observers.len() }
    pub fn values(&self) -> Vec<Arc<T>> { lock(&self.data).entries.values().cloned().collect() }
    pub fn get(&self, key: &JsString) -> Option<Arc<T>> { lock(&self.data).entries.get(key).cloned() }
    pub fn insert(&self, entry: Arc<T>) -> Result<(), ServiceError> {
        { let mut d = lock(&self.data); if d.disposed { return Err(contract("Keyed service directory is disposed")); } if d.entries.contains_key(entry.key()) { return Err(contract("Keyed service already has a live instance")); } d.entries.insert(entry.key().clone(), entry.clone()); }
        self.start_all(&entry); Ok(())
    }
    pub fn replace(&self, entry: Arc<T>) -> Result<(), ServiceError> {
        if let Some(previous) = self.get(entry.key()) { if previous.generation() == entry.generation() { return Err(contract("Keyed service repeated a live generation")); } self.remove(&previous); }
        self.insert(entry)
    }
    pub fn remove(&self, entry: &Arc<T>) {
        let removed = {
            let mut d = lock(&self.data);
            if !d.entries.get(entry.key()).is_some_and(|e| Arc::ptr_eq(e, entry)) { return; }
            let removed = d.entries.remove(entry.key());
            for (_, observer) in &mut d.observers { if let Some(task) = observer.tasks.remove(entry.key()) { task.cancel(); } }
            removed
        };
        if let Some(entry) = removed { entry.deactivate(); }
    }
    pub fn ready(&self) -> Result<(), ServiceError> {
        { let mut d = lock(&self.data); if d.disposed { return Err(contract("Keyed service directory is disposed")); } if d.ready { return Ok(()); } d.ready = true; }
        for entry in self.values() { self.start_all(&entry); } Ok(())
    }
    pub fn reset(&self) { lock(&self.data).ready = false; for e in self.values() { self.remove(&e); } }
    pub fn observe(self: &Arc<Self>, handler: ServiceObserver) -> Result<super::state_internals::Unsubscribe, ServiceError> {
        let id = Arc::new(());
        { let mut d = lock(&self.data); if d.disposed { return Err(contract("Keyed service directory is disposed")); } d.observers.push((id.clone(), Observer { handler, tasks: BTreeMap::new() })); }
        for entry in self.values() { self.start_all(&entry); }
        let directory = Arc::downgrade(self);
        Ok(Box::new(move || { if let Some(directory) = directory.upgrade() { let mut d = lock(&directory.data); if let Some(index) = d.observers.iter().position(|(key, _)| Arc::ptr_eq(key, &id)) { let (_, observer) = d.observers.remove(index); for (_, task) in observer.tasks { task.cancel(); } } } }))
    }
    pub fn dispose(&self) { { let mut d = lock(&self.data); if d.disposed { return; } d.disposed = true; for (_, observer) in d.observers.drain(..) { for (_, task) in observer.tasks { task.cancel(); } } } self.reset(); }
    fn start_all(&self, entry: &Arc<T>) {
        let jobs = {
            let mut d = lock(&self.data);
            if !d.ready || d.disposed { return; }
            d.observers.iter_mut().filter_map(|(_, observer)| {
                if observer.tasks.contains_key(entry.key()) { return None; }
                let (context, cancel) = with_cancel(&BACKGROUND_CONTEXT);
                observer.tasks.insert(entry.key().clone(), cancel);
                Some((observer.handler.clone(), context))
            }).collect::<Vec<_>>()
        };
        for (handler, context) in jobs {
            if context.abort_signal().is_some_and(|signal| signal.aborted()) { continue; }
            let future = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(entry.service(), context.clone()))) {
                Ok(future) => future,
                Err(_) => { report_error(&self.report, contract("Instance observer panicked")); continue; }
            };
            let future: BoxFuture<'static, Result<(), ServiceError>> = Box::pin(async move {
                match std::panic::AssertUnwindSafe(future).catch_unwind().await { Ok(result) => result, Err(_) => Err(contract("Instance observer future panicked")) }
            });
            let report = self.report.clone();
            let mut future = future;
            match future.as_mut().now_or_never() {
                Some(result) => { if let Err(error) = result { if !context.abort_signal().is_some_and(|s| s.aborted()) { report_error(&report, error); } } }
                None => { tokio::spawn(async move { if let Err(error) = future.await { if !context.abort_signal().is_some_and(|s| s.aborted()) { report_error(&report, error); } } }); }
            }
        }
    }
}
