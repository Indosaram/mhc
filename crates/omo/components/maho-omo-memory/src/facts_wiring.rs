use std::sync::Arc;
use memory_core::{identity::layout::MemoryIdentityPaths, facts::queue::{FactsAbortSignal, FactsEnqueueRequest, FactsEnqueueResult, FactsQueue, FactsQueueOptions}, facts::schema::FactsQueueEntry, journal::store::{TranscriptJournal, TranscriptJournalOptions}};

pub trait FactsExtractorPort {
    fn launch_pending(&mut self, signal: Option<&FactsAbortSignal>) -> Result<(), String>;
    fn reconcile_pending(&mut self, signal: Option<&FactsAbortSignal>) -> Result<(), String>;
}

pub struct MemoryFactsWiringOptions {
    pub identity: String,
    pub identity_paths: MemoryIdentityPaths,
    pub facts_enabled: Box<dyn Fn() -> bool>,
    pub debounce_settles: Box<dyn Fn() -> usize>,
    pub extractor: Option<Box<dyn FactsExtractorPort>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub warn: Box<dyn Fn(&str)>,
}

pub struct MemoryFactsWiring { options: MemoryFactsWiringOptions, queue: FactsQueue, settles: usize }

pub fn create_memory_facts_wiring(options: MemoryFactsWiringOptions) -> MemoryFactsWiring {
    let queue = FactsQueue::new(FactsQueueOptions { identity_paths:options.identity_paths.clone(), now:options.now.clone(), on_publish:None });
    MemoryFactsWiring { options, queue, settles:0 }
}

fn disabled() -> FactsEnqueueResult { FactsEnqueueResult::NotEnqueued { reason:"no-new-entries".into() } }

impl MemoryFactsWiring {
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
            if let Some(extractor) = &mut self.options.extractor && let Err(error) = extractor.launch_pending(None) { (self.options.warn)(&format!("facts extractor launch failed: {error}")); }
        }
        result
    }
    pub fn reconcile_pending(&self) -> Vec<FactsQueueEntry> {
        if !(self.options.facts_enabled)() { return vec![]; }
        match self.queue.list_pending() { Ok(entries) => entries, Err(error) => { (self.options.warn)(&format!("facts queue reconcile failed: {error}")); vec![] } }
    }
    pub fn reconcile_extractor(&mut self) {
        if !(self.options.facts_enabled)() { return; }
        if let Some(extractor) = &mut self.options.extractor && let Err(error) = extractor.reconcile_pending(None) { (self.options.warn)(&format!("facts extractor reconcile failed: {error}")); }
    }
    pub fn launch_if_threshold_met(&mut self, signal: Option<&FactsAbortSignal>) -> bool {
        if signal.is_some_and(FactsAbortSignal::is_aborted) || !(self.options.facts_enabled)() || self.settles < (self.options.debounce_settles)().max(1) { return false; }
        if signal.is_some_and(FactsAbortSignal::is_aborted) { return false; }
        let Some(extractor) = &mut self.options.extractor else { return false; };
        self.settles = 0;
        if let Err(error) = extractor.launch_pending(signal) { (self.options.warn)(&format!("facts extractor launch failed: {error}")); }
        true
    }
    pub fn mark_consumed(&self, entries: &[FactsQueueEntry]) {
        if let Err(error) = self.queue.mark_consumed(entries) { (self.options.warn)(&format!("facts queue consume failed: {error}")); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Extractor(Rc<Cell<usize>>);
    impl FactsExtractorPort for Extractor { fn launch_pending(&mut self, _: Option<&FactsAbortSignal>) -> Result<(), String> { self.0.set(self.0.get() + 1); Ok(()) } fn reconcile_pending(&mut self, _: Option<&FactsAbortSignal>) -> Result<(), String> { self.0.set(self.0.get() + 10); Ok(()) } }
    fn fixture(root: &std::path::Path, enabled: bool, threshold: Rc<Cell<usize>>) -> (MemoryFactsWiring, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        let options = MemoryFactsWiringOptions { identity:"agent".into(), identity_paths:memory_core::identity::layout::build_identity_paths(root, "agent"), facts_enabled:Box::new(move || enabled), debounce_settles:Box::new(move || threshold.get()), extractor:Some(Box::new(Extractor(calls.clone()))), now:Some(Arc::new(||0)), warn:Box::new(|error| panic!("{error}")) };
        (create_memory_facts_wiring(options), calls)
    }
    #[test] fn debounce_counts_settles_even_without_new_entries() { let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(4))); for _ in 0..3 { assert!(!wiring.on_settled("conversation").is_enqueued()); } assert_eq!(calls.get(),0); wiring.on_settled("conversation"); assert_eq!(calls.get(),1); }
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
    #[test] fn cancellation_during_threshold_probe_prevents_spawn() {
        let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(4)));
        wiring.on_settled("conversation"); let signal=FactsAbortSignal::new(); let injected=signal.clone();
        wiring.options.debounce_settles=Box::new(move || { injected.abort(); 1 });
        assert!(!wiring.launch_if_threshold_met(Some(&signal))); assert_eq!(calls.get(),0);
    }
    #[test] fn disabled_never_launches_or_reconciles() { let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),false,Rc::new(Cell::new(1))); wiring.on_settled("conversation"); wiring.reconcile_extractor(); assert!(wiring.reconcile_pending().is_empty()); assert_eq!(calls.get(),0); }
    #[test] fn lowered_threshold_allows_shutdown_launch_once() { let root=tempfile::tempdir().unwrap(); let threshold=Rc::new(Cell::new(4)); let (mut wiring,calls)=fixture(root.path(),true,threshold.clone()); wiring.on_settled("conversation"); threshold.set(1); assert!(wiring.launch_if_threshold_met(None)); assert!(!wiring.launch_if_threshold_met(None)); assert_eq!(calls.get(),1); }
    #[test] fn aborted_signal_prevents_enqueue_and_threshold_launch() { let root=tempfile::tempdir().unwrap(); let threshold=Rc::new(Cell::new(4)); let (mut wiring,calls)=fixture(root.path(),true,threshold.clone()); wiring.on_settled("conversation"); threshold.set(1); let signal=FactsAbortSignal::new(); signal.abort(); assert!(!wiring.enqueue_settled("conversation",Some(&signal)).is_enqueued()); assert!(!wiring.launch_if_threshold_met(Some(&signal))); assert_eq!(calls.get(),0); assert!(wiring.launch_if_threshold_met(None)); }
}
