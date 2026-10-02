use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}, mpsc}, thread};
use notify::{Event, RecursiveMode, Watcher};
use futures::{FutureExt, future::{BoxFuture, Shared}};

pub type WatchEventListener = Arc<dyn Fn(&str, Option<PathBuf>) + Send + Sync>;
pub type WatchErrorListener = Arc<dyn Fn(String, PathBuf) + Send + Sync>;
pub type NativeEventCallback = Box<dyn FnMut(notify::Result<Event>) + Send>;
pub type WatcherFactory = Arc<dyn Fn(&std::path::Path, bool, NativeEventCallback) -> notify::Result<Box<dyn Send>> + Send + Sync>;
#[derive(Clone)]
struct Subscription { path: PathBuf, recursive: bool, active: Arc<AtomicBool>, listener: WatchEventListener, on_error: WatchErrorListener }
enum Command { Watch(u64, Subscription), Unwatch(u64), Event(u64, notify::Result<Event>), Barrier(mpsc::Sender<()>), Shutdown }
struct Worker { sender: mpsc::Sender<Command>, join: thread::JoinHandle<()> }
struct Registry { worker: Option<Worker>, count: usize, next_id: u64, factory: WatcherFactory, offloaded: bool }
impl Default for Registry {
    fn default() -> Self { Self { worker: None, count: 0, next_id: 0, offloaded: cfg!(any(target_os = "linux", target_os = "macos")), factory: Arc::new(|path, recursive, callback| {
        let mut watcher = notify::recommended_watcher(callback)?;
        watcher.watch(path, if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive })?;
        Ok(Box::new(watcher))
    }) } }
}
static REGISTRY: OnceLock<Arc<Mutex<Registry>>> = OnceLock::new();
#[derive(Clone, Default)]
pub struct FsWatchEventSource { registry: Arc<Mutex<Registry>> }
impl FsWatchEventSource {
    pub fn with_platform_factory(platform: &str, factory: WatcherFactory) -> Self { Self { registry: Arc::new(Mutex::new(Registry { factory, offloaded: matches!(platform, "linux" | "darwin"), ..Default::default() })) } }
    pub fn with_factory(factory: WatcherFactory) -> Self { Self { registry: Arc::new(Mutex::new(Registry { factory, ..Default::default() })) } }
    pub fn shared() -> Self { Self { registry: Arc::clone(REGISTRY.get_or_init(|| Arc::new(Mutex::default()))) } }
    pub fn subscribe(&self, path: PathBuf, recursive: bool, listener: WatchEventListener, on_error: WatchErrorListener) -> Result<WatchSubscription, String> {
        subscribe_in(Arc::clone(&self.registry), path, recursive, listener, on_error)
    }
}
pub struct WatchSubscription { id: u64, active: Arc<AtomicBool>, closed: bool, registry: Arc<Mutex<Registry>>, close_completion: Option<Shared<BoxFuture<'static, Result<(), String>>>>, direct: Option<Box<dyn Send>> }
impl WatchSubscription {
    pub fn ready(&self) -> Result<(), String> {
        if self.direct.is_some() { return Ok(()); }
        let (sender, receiver) = mpsc::channel();
        let registry = self.registry.lock().map_err(|error| error.to_string())?;
        if let Some(worker) = &registry.worker { worker.sender.send(Command::Barrier(sender)).map_err(|error| error.to_string())?; }
        drop(registry);
        receiver.recv_timeout(std::time::Duration::from_secs(5)).map_err(|error| error.to_string())
    }
    pub fn close(&mut self) -> Result<(), String> {
        if let Some(worker) = self.cancel()? { worker.join.join().map_err(|_| "Config watcher teardown failed".to_owned())?; }
        Ok(())
    }
    pub fn close_async(&mut self) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static + use<> {
        if let Some(completion) = &self.close_completion { return completion.clone(); }
        let worker = self.cancel();
        let completion = async move {
            if let Some(worker) = worker? {
                tokio::task::spawn_blocking(move || worker.join.join().map_err(|_| "Config watcher teardown failed".to_owned())).await.map_err(|error| error.to_string())??;
            }
            Ok(())
        }.boxed().shared();
        self.close_completion = Some(completion.clone());
        completion
    }
    fn cancel(&mut self) -> Result<Option<Worker>, String> {
        if self.closed { return Ok(None); }
        self.closed = true;
        self.active.store(false, Ordering::SeqCst);
        if self.direct.take().is_some() { return Ok(None); }
        let worker = {
            let mut registry = self.registry.lock().map_err(|error| error.to_string())?;
            if let Some(worker) = &registry.worker { worker.sender.send(Command::Unwatch(self.id)).map_err(|error| error.to_string())?; }
            registry.count -= 1;
            if registry.count == 0 { registry.worker.take() } else { None }
        };
        if let Some(worker) = &worker {
            worker.sender.send(Command::Shutdown).map_err(|error| error.to_string())?;
        }
        Ok(worker)
    }
}
impl Drop for WatchSubscription { fn drop(&mut self) { if let Err(error) = self.close() { eprintln!("{error}"); } } }
pub fn subscribe(path: PathBuf, recursive: bool, listener: WatchEventListener, on_error: WatchErrorListener) -> Result<WatchSubscription, String> {
    FsWatchEventSource::shared().subscribe(path, recursive, listener, on_error)
}
fn subscribe_in(owner: Arc<Mutex<Registry>>, path: PathBuf, recursive: bool, listener: WatchEventListener, on_error: WatchErrorListener) -> Result<WatchSubscription, String> {
    let mut registry = owner.lock().map_err(|error| error.to_string())?;
    if !registry.offloaded {
        let factory = Arc::clone(&registry.factory);
        drop(registry);
        let active = Arc::new(AtomicBool::new(true));
        let subscription = Subscription { path: path.clone(), recursive, active: Arc::clone(&active), listener, on_error };
        let watcher = factory(&path, recursive, Box::new(move |event| {
            if !subscription.active.load(Ordering::SeqCst) { return; }
            match event {
                Err(error) => (subscription.on_error)(error.to_string(), subscription.path.clone()),
                Ok(event) => {
                    let event_type = if matches!(event.kind, notify::EventKind::Create(_) | notify::EventKind::Remove(_) | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))) { "rename" } else { "change" };
                    if event.paths.is_empty() { (subscription.listener)(event_type, None); }
                    for path in event.paths {
                        if let Ok(relative) = path.strip_prefix(&subscription.path)
                            && (subscription.recursive || relative.components().count() <= 1) { (subscription.listener)(event_type, Some(relative.into())); }
                    }
                },
            }
        })).map_err(|error| error.to_string())?;
        return Ok(WatchSubscription { id: 0, active, closed: false, registry: owner, close_completion: None, direct: Some(watcher) });
    }
    if registry.worker.is_none() {
        let (sender, receiver) = mpsc::channel();
        let event_sender = sender.clone();
        let factory = Arc::clone(&registry.factory);
        let join = thread::Builder::new().name("config-watch".into()).spawn(move || run_worker_with_factory(receiver, event_sender, factory)).map_err(|error| error.to_string())?;
        registry.worker = Some(Worker { sender, join });
    }
    registry.next_id += 1;
    let id = registry.next_id;
    let active = Arc::new(AtomicBool::new(true));
    let subscription = Subscription { path, recursive, active: Arc::clone(&active), listener, on_error };
    if let Some(worker) = &registry.worker { worker.sender.send(Command::Watch(id, subscription)).map_err(|error| error.to_string())?; }
    registry.count += 1;
    drop(registry);
    Ok(WatchSubscription { id, active, closed: false, registry: owner, close_completion: None, direct: None })
}
#[cfg(test)]
fn run_worker(receiver: mpsc::Receiver<Command>, sender: mpsc::Sender<Command>) {
    run_worker_with_factory(receiver, sender, Registry::default().factory);
}
fn run_worker_with_factory(receiver: mpsc::Receiver<Command>, sender: mpsc::Sender<Command>, factory: WatcherFactory) {
    let mut subscriptions: BTreeMap<u64, (Subscription, Box<dyn Send>)> = BTreeMap::new();
    while let Ok(command) = receiver.recv() {
        let shutdown = matches!(command, Command::Shutdown);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        match command {
            Command::Shutdown => {},
            Command::Barrier(sender) => { let _ = sender.send(()); },
            Command::Unwatch(id) => { subscriptions.remove(&id); },
            Command::Watch(id, subscription) => {
                if !subscription.active.load(Ordering::SeqCst) { return; }
                let sender = sender.clone();
                let watcher = factory(&subscription.path, subscription.recursive, Box::new(move |event| { let _ = sender.send(Command::Event(id, event)); }));
                match watcher {
                    Ok(watcher) => { if subscription.active.load(Ordering::SeqCst) { subscriptions.insert(id, (subscription, watcher)); } },
                    Err(error) => (subscription.on_error)(error.to_string(), subscription.path),
                }
            },
            Command::Event(id, event) => {
                if let Some((subscription, _)) = subscriptions.get(&id) {
                    if !subscription.active.load(Ordering::SeqCst) { return; }
                    match &event {
                        Err(error) => (subscription.on_error)(error.to_string(), subscription.path.clone()),
                        Ok(event) => {
                            let event_type = if matches!(event.kind, notify::EventKind::Create(_) | notify::EventKind::Remove(_) | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))) { "rename" } else { "change" };
                            if event.paths.is_empty() { (subscription.listener)(event_type, None); }
                            for path in &event.paths {
                                if let Ok(relative) = path.strip_prefix(&subscription.path)
                                    && (subscription.recursive || relative.components().count() <= 1) { (subscription.listener)(event_type, Some(relative.into())); }
                            }
                        },
                    }
                }
            },
        }
        }));
        if shutdown { break; }
        if result.is_err() {
            let surviving: Vec<_> = subscriptions.iter().map(|(id, (subscription, _))| (*id, subscription.clone())).collect();
            subscriptions.clear();
            for (id, subscription) in surviving {
                if !subscription.active.load(Ordering::SeqCst) { continue; }
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (subscription.on_error)("Config watcher worker crashed".into(), subscription.path.clone())));
                let sender = sender.clone();
                match factory(&subscription.path, subscription.recursive, Box::new(move |event| { let _ = sender.send(Command::Event(id, event)); })) {
                    Ok(watcher) => { subscriptions.insert(id, (subscription, watcher)); },
                    Err(error) => (subscription.on_error)(error.to_string(), subscription.path),
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pathless_events_request_a_full_rescan() {
        let root = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::channel();
        let event_sender = sender.clone();
        let worker = thread::spawn(move || run_worker(receiver, event_sender));
        let (events, received) = mpsc::channel();
        sender.send(Command::Watch(1, Subscription { path: root.path().into(), recursive: false, active: Arc::new(AtomicBool::new(true)), listener: Arc::new(move |kind, path| { events.send((kind.to_owned(), path)).unwrap(); }), on_error: Arc::new(|error, _| panic!("{error}")) })).unwrap();
        let (ready, barrier) = mpsc::channel();
        sender.send(Command::Barrier(ready)).unwrap();
        barrier.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        sender.send(Command::Event(1, Ok(Event::new(notify::EventKind::Any)))).unwrap();
        assert_eq!(received.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), ("change".into(), None));
        sender.send(Command::Shutdown).unwrap();
        worker.join().unwrap();
    }
}
