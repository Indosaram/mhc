use std::sync::Arc;
use memory_core::{identity::layout::MemoryIdentityPaths, facts::queue::{FactsAbortSignal, FactsEnqueueRequest, FactsEnqueueResult, FactsQueue, FactsQueueOptions}, facts::schema::FactsQueueEntry, journal::store::{TranscriptJournal, TranscriptJournalOptions}};

pub type FactsExtractorWork = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
pub trait FactsExtractorPort: Send {
    fn launch_pending(&mut self, signal: Option<&FactsAbortSignal>) -> FactsExtractorWork;
    fn reconcile_pending(&mut self, signal: Option<&FactsAbortSignal>) -> FactsExtractorWork;
}

pub struct MemoryFactsWiringOptions {
    pub identity: String,
    pub identity_paths: MemoryIdentityPaths,
    pub facts_enabled: Box<dyn Fn() -> bool + Send + Sync>,
    pub debounce_settles: Box<dyn Fn() -> usize + Send + Sync>,
    pub extractor: Option<Box<dyn FactsExtractorPort>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

pub struct MemoryFactsWiring { options: MemoryFactsWiringOptions, queue: FactsQueue, settles: usize }

pub fn create_memory_facts_wiring(options: MemoryFactsWiringOptions) -> MemoryFactsWiring {
    let queue = FactsQueue::new(FactsQueueOptions { identity_paths:options.identity_paths.clone(), now:options.now.clone(), on_publish:None });
    MemoryFactsWiring { options, queue, settles:0 }
}

fn disabled() -> FactsEnqueueResult { FactsEnqueueResult::NotEnqueued { reason:"no-new-entries".into() } }

impl MemoryFactsWiring {
    fn fire(&mut self, reconcile: bool) {
        let Some(extractor) = &mut self.options.extractor else { return; };
        let work = if reconcile { extractor.reconcile_pending(None) } else { extractor.launch_pending(None) };
        let warn = self.options.warn.clone();
        tokio::spawn(async move {
            if let Err(error) = work.await { warn(&format!("facts extractor {} failed: {error}", if reconcile { "reconcile" } else { "launch" })); }
        });
    }
    pub fn enqueue_settled(&self, conversation: &str, signal: Option<&FactsAbortSignal>) -> FactsEnqueueResult {
        if signal.is_some_and(FactsAbortSignal::is_aborted) || !(self.options.facts_enabled)() { return disabled(); }
        let enqueue = || {
            let journal = TranscriptJournal::new(TranscriptJournalOptions::new(self.options.identity_paths.transcripts.join(conversation)));
            let entries = journal.read_entries().map_err(|error| error.to_string())?;
            if signal.is_some_and(FactsAbortSignal::is_aborted) || entries.is_empty() { return Ok(disabled()); }
            self.queue.enqueue(FactsEnqueueRequest { identity:self.options.identity.clone(), session_id:conversation.into(), conversation_id:conversation.into(), entries, signal:signal.cloned() }).map_err(|error| error.to_string())
        };
        match enqueue() { Ok(result) => result, Err(error) => { (self.options.warn)(&format!("facts queue enqueue failed: {conversation}: {error}")); disabled() } }
    }
    pub fn on_settled(&mut self, conversation: &str) -> FactsEnqueueResult {
        let result = self.enqueue_settled(conversation, None);
        if !(self.options.facts_enabled)() { return result; }
        self.settles += 1;
        if self.settles >= (self.options.debounce_settles)().max(1) {
            self.settles = 0;
            self.fire(false);
        }
        result
    }
    pub fn reconcile_pending(&self) -> Vec<FactsQueueEntry> {
        if !(self.options.facts_enabled)() { return vec![]; }
        match self.queue.list_pending() { Ok(entries) => entries, Err(error) => { (self.options.warn)(&format!("facts queue reconcile failed: {error}")); vec![] } }
    }
    pub fn reconcile_extractor(&mut self) {
        if !(self.options.facts_enabled)() { return; }
        self.fire(true);
    }
    pub async fn launch_if_threshold_met(&mut self, signal: Option<&FactsAbortSignal>) -> bool {
        if signal.is_some_and(FactsAbortSignal::is_aborted) || !(self.options.facts_enabled)() || self.settles < (self.options.debounce_settles)().max(1) { return false; }
        if signal.is_some_and(FactsAbortSignal::is_aborted) { return false; }
        let Some(extractor) = &mut self.options.extractor else { return false; };
        self.settles = 0;
        if signal.is_some_and(FactsAbortSignal::is_aborted) { return false; }
        if let Err(error) = extractor.launch_pending(signal).await { (self.options.warn)(&format!("facts extractor launch failed: {error}")); }
        true
    }
    pub fn mark_consumed(&self, entries: &[FactsQueueEntry]) {
        if let Err(error) = self.queue.mark_consumed(entries) { (self.options.warn)(&format!("facts queue consume failed: {error}")); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::Arc as Rc;
    #[derive(Default)]
    struct Cell(std::sync::atomic::AtomicUsize);
    impl Cell {
        fn new(value:usize)->Self{Self(std::sync::atomic::AtomicUsize::new(value))}
        fn get(&self)->usize{self.0.load(std::sync::atomic::Ordering::SeqCst)}
        fn set(&self,value:usize){self.0.store(value,std::sync::atomic::Ordering::SeqCst);}
    }
    struct Extractor(Rc<Cell>);
    impl FactsExtractorPort for Extractor { fn launch_pending(&mut self, _: Option<&FactsAbortSignal>) -> FactsExtractorWork { self.0.set(self.0.get() + 1); Box::pin(std::future::ready(Ok(()))) } fn reconcile_pending(&mut self, _: Option<&FactsAbortSignal>) -> FactsExtractorWork { self.0.set(self.0.get() + 10); Box::pin(std::future::ready(Ok(()))) } }
    fn fixture(root: &std::path::Path, enabled: bool, threshold: Rc<Cell>) -> (MemoryFactsWiring, Rc<Cell>) {
        let calls = Rc::new(Cell::new(0));
        let options = MemoryFactsWiringOptions { identity:"agent".into(), identity_paths:memory_core::identity::layout::build_identity_paths(root, "agent"), facts_enabled:Box::new(move || enabled), debounce_settles:Box::new(move || threshold.get()), extractor:Some(Box::new(Extractor(calls.clone()))), now:Some(Arc::new(||0)), warn:Arc::new(|error| panic!("{error}")) };
        (create_memory_facts_wiring(options), calls)
    }
    struct DeferredExtractor(Option<tokio::sync::oneshot::Receiver<()>>);
    impl FactsExtractorPort for DeferredExtractor {
        fn launch_pending(&mut self, _: Option<&FactsAbortSignal>) -> FactsExtractorWork {
            let released = self.0.take().unwrap();
            Box::pin(async move { released.await.map_err(|error| error.to_string()) })
        }
        fn reconcile_pending(&mut self, signal: Option<&FactsAbortSignal>) -> FactsExtractorWork { self.launch_pending(signal) }
    }
    #[tokio::test]
    async fn shutdown_launch_waits_for_extractor_completion() {
        let root = tempfile::tempdir().unwrap();
        let threshold = Rc::new(Cell::new(4));
        let (mut wiring, _) = fixture(root.path(), true, threshold.clone());
        wiring.on_settled("conversation");
        threshold.set(1);
        let (release, released) = tokio::sync::oneshot::channel();
        wiring.options.extractor = Some(Box::new(DeferredExtractor(Some(released))));
        let launched = wiring.launch_if_threshold_met(None);
        tokio::pin!(launched);
        assert!(std::future::poll_fn(|context| std::task::Poll::Ready(launched.as_mut().poll(context).is_pending())).await);
        release.send(()).unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_secs(5), launched).await.unwrap());
    }
    #[tokio::test]
    async fn reconcile_returns_before_extractor_and_reports_async_failure() {
        let root = tempfile::tempdir().unwrap();
        let (mut wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let (release, released) = tokio::sync::oneshot::channel();
        wiring.options.extractor = Some(Box::new(DeferredExtractor(Some(released))));
        let (reported, report) = tokio::sync::oneshot::channel();
        let reported = std::sync::Mutex::new(Some(reported));
        wiring.options.warn = Arc::new(move |error| { reported.lock().unwrap().take().unwrap().send(error.to_owned()).unwrap(); });
        wiring.reconcile_extractor();
        drop(release);
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), report).await.unwrap().unwrap();
        assert!(!error.is_empty());
    }
    #[tokio::test] async fn debounce_counts_settles_even_without_new_entries() { let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(4))); for _ in 0..3 { assert!(!wiring.on_settled("conversation").is_enqueued()); } assert_eq!(calls.get(),0); wiring.on_settled("conversation"); assert_eq!(calls.get(),1); }
    #[test] fn real_journal_delta_enqueues_once_and_consumes() {
        use memory_core::journal::entries::{TranscriptEntry, TextTranscriptEntry};
        let root=tempfile::tempdir().unwrap(); let (wiring,_)=fixture(root.path(),true,Rc::new(Cell::new(4)));
        let journal=TranscriptJournal::new(TranscriptJournalOptions::new(wiring.options.identity_paths.transcripts.join("conversation")));
        journal.append(&[TranscriptEntry::Text(TextTranscriptEntry::new("user","Question","2026-01-01T00:00:00.000Z","m:user","m")),TranscriptEntry::Text(TextTranscriptEntry::new("assistant","Answer","2026-01-01T00:00:00.000Z","m:assistant","m"))]).unwrap();
        assert!(wiring.enqueue_settled("conversation",None).is_enqueued());
        assert!(!wiring.enqueue_settled("conversation",None).is_enqueued());
        let pending=wiring.reconcile_pending(); assert_eq!(pending.len(),1); assert_eq!(pending[0].conversation_id,"conversation");
        wiring.mark_consumed(&pending); assert!(wiring.reconcile_pending().is_empty()); assert!(!wiring.enqueue_settled("conversation",None).is_enqueued());
    }
    #[tokio::test] async fn cancellation_during_threshold_probe_prevents_spawn() {
        let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(4)));
        wiring.on_settled("conversation"); let signal=FactsAbortSignal::new(); let injected=signal.clone();
        wiring.options.debounce_settles=Box::new(move || { injected.abort(); 1 });
        assert!(!wiring.launch_if_threshold_met(Some(&signal)).await); assert_eq!(calls.get(),0);
    }
    #[test] fn disabled_never_launches_or_reconciles() { let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),false,Rc::new(Cell::new(1))); wiring.on_settled("conversation"); wiring.reconcile_extractor(); assert!(wiring.reconcile_pending().is_empty()); assert_eq!(calls.get(),0); }
    #[tokio::test] async fn lowered_threshold_allows_shutdown_launch_once() { let root=tempfile::tempdir().unwrap(); let threshold=Rc::new(Cell::new(4)); let (mut wiring,calls)=fixture(root.path(),true,threshold.clone()); wiring.on_settled("conversation"); threshold.set(1); assert!(wiring.launch_if_threshold_met(None).await); assert!(!wiring.launch_if_threshold_met(None).await); assert_eq!(calls.get(),1); }
    #[tokio::test] async fn aborted_signal_prevents_enqueue_and_threshold_launch() { let root=tempfile::tempdir().unwrap(); let threshold=Rc::new(Cell::new(4)); let (mut wiring,calls)=fixture(root.path(),true,threshold.clone()); wiring.on_settled("conversation"); threshold.set(1); let signal=FactsAbortSignal::new(); signal.abort(); assert!(!wiring.enqueue_settled("conversation",Some(&signal)).is_enqueued()); assert!(!wiring.launch_if_threshold_met(Some(&signal)).await); assert_eq!(calls.get(),0); assert!(wiring.launch_if_threshold_met(None).await); }
}
