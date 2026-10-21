use maho_chord::{services::instances::*, types::*};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
struct Entry { key: JsString, generation: ServiceSequence, active: AtomicBool }
impl InstanceDirectoryEntry for Entry {
    fn key(&self) -> &JsString { &self.key }
    fn generation(&self) -> ServiceSequence { self.generation }
    fn service(&self) -> ServiceImplementation { Arc::new(self.generation) }
    fn deactivate(&self) { self.active.store(false, Ordering::Release); }
}
#[tokio::test]
async fn deferred_readiness_generation_replacement_reset_and_disposal() -> Result<(), ServiceError> {
    let directory = InstanceDirectory::new(false, Arc::new(|e| panic!("{e}")));
    let first = Arc::new(Entry { key: "key".into(), generation: seq(1), active: AtomicBool::new(true) });
    directory.insert(first.clone())?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    let stop = directory.observe(Arc::new(move |service, context| {
        let sender = sender.clone(); Box::pin(async move { sender.send((service,context)).await.map_err(|e| e.to_string())?; Ok(()) })
    }))?;
    assert_eq!(directory.observer_count(), 1);
    assert!(matches!(receiver.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
    directory.ready()?; directory.ready()?;
    let (service, context) = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await?.ok_or("missing observation")?;
    assert_eq!(service.downcast_ref::<ServiceSequence>(), Some(&seq(1)));
    assert!(directory.replace(Arc::new(Entry { key: "key".into(), generation: seq(1), active: AtomicBool::new(true) })).is_err());
    let second = Arc::new(Entry { key: "key".into(), generation: seq(2), active: AtomicBool::new(true) });
    directory.replace(second.clone())?;
    assert!(!first.active.load(Ordering::Acquire));
    assert!(context.abort_signal().ok_or("missing signal")?.aborted());
    directory.remove(&first);
    assert_eq!(directory.get(&"key".into()).ok_or("missing replacement")?.generation(), seq(2));
    let (_, second_context) = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await?.ok_or("missing second observation")?;
    directory.reset();
    assert!(second_context.abort_signal().ok_or("missing second signal")?.aborted());
    assert!(!second.active.load(Ordering::Acquire));
    assert!(directory.values().is_empty());
    stop();
    assert_eq!(directory.observer_count(), 0);
    directory.dispose();
    assert!(directory.ready().is_err()); Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
