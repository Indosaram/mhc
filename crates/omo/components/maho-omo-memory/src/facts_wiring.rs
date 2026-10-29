use std::sync::Arc;
use memory_core::{identity::layout::MemoryIdentityPaths, facts::queue::{FactsAbortSignal, FactsEnqueueRequest, FactsEnqueueResult, FactsQueue, FactsQueueOptions}, facts::schema::FactsQueueEntry, journal::store::{TranscriptJournal, TranscriptJournalOptions}};

pub type FactsExtractorWork = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
pub trait FactsExtractorPort: Send {
    fn launch_pending(&mut self, signal: Option<&FactsAbortSignal>) -> FactsExtractorWork;
    fn reconcile_pending(&mut self, signal: Option<&FactsAbortSignal>) -> FactsExtractorWork;
    fn cancel_active(&mut self) -> FactsExtractorWork { Box::pin(std::future::ready(Ok(()))) }
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

pub struct MemoryFactsWiring {
    options: MemoryFactsWiringOptions,
    queue: FactsQueue,
    settles: usize,
    settled_sessions: std::collections::BTreeSet<String>,
    pub(crate) provenances: std::sync::RwLock<std::collections::HashMap<String, maho_ext_api::ForkProvenance>>,
}

pub fn create_memory_facts_wiring(options: MemoryFactsWiringOptions) -> MemoryFactsWiring {
    let queue = FactsQueue::new(FactsQueueOptions {
        identity_paths: options.identity_paths.clone(),
        now: options.now.clone(),
        on_publish: None,
    });
    MemoryFactsWiring {
        options,
        queue,
        settles: 0,
        settled_sessions: Default::default(),
        provenances: std::sync::RwLock::new(Default::default()),
    }
}

fn disabled() -> FactsEnqueueResult { FactsEnqueueResult::NotEnqueued { reason:"no-new-entries".into() } }

impl MemoryFactsWiring {
    fn fire(&mut self, reconcile: bool) {
        let Some(extractor) = &mut self.options.extractor else { return; };
        let work = if reconcile { extractor.reconcile_pending(None) } else { extractor.launch_pending(None) };
        let warn = self.options.warn.clone();
        let kind = if reconcile { "reconcile" } else { "launch" };
        // The extractor scans and parses every retained queue file synchronously. The interactive
        // host runs a current-thread runtime, so a task spawned there froze the UI for the whole
        // scan on session start and `/new`; the work gets its own thread and runtime instead.
        let thread_warn = warn.clone();
        let spawned = std::thread::Builder::new().name("memory-facts".into()).spawn(move || {
            match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => if let Err(error) = runtime.block_on(work) { thread_warn(&format!("facts extractor {kind} failed: {error}")); },
                Err(error) => thread_warn(&format!("facts extractor {kind} failed: {error}")),
            }
        });
        if let Err(error) = spawned { warn(&format!("facts extractor {kind} failed: {error}")); }
    }
    pub fn set_provenance(&self, session: &str, provenance: Option<maho_ext_api::ForkProvenance>) {
        if let Ok(mut map) = self.provenances.write() {
            if let Some(prov) = provenance {
                map.insert(session.to_owned(), prov);
            } else {
                map.remove(session);
            }
        }
    }
    pub fn provenance(&self, session: &str) -> Option<maho_ext_api::ForkProvenance> {
        self.provenances.read().ok().and_then(|map| map.get(session).cloned())
    }
    pub fn enqueue_settled(&self, conversation: &str, signal: Option<&FactsAbortSignal>) -> FactsEnqueueResult {
        let provenance = self.provenance(conversation);
        self.enqueue_settled_with_provenance(conversation, signal, provenance.as_ref())
    }
    pub fn enqueue_settled_with_provenance(
        &self,
        conversation: &str,
        signal: Option<&FactsAbortSignal>,
        provenance: Option<&maho_ext_api::ForkProvenance>,
    ) -> FactsEnqueueResult {
        if signal.is_some_and(FactsAbortSignal::is_aborted) || !(self.options.facts_enabled)() { return disabled(); }
        let enqueue = || {
            let journal = TranscriptJournal::new(TranscriptJournalOptions::new(self.options.identity_paths.transcripts.join(conversation)));
            let entries = journal.read_entries().map_err(|error| error.to_string())?;
            if signal.is_some_and(FactsAbortSignal::is_aborted) || entries.is_empty() { return Ok(disabled()); }

            if let Some(prov) = provenance
                && let Ok(cursor) = self.queue.read_cursor(conversation)
            {
                let has_cursor = cursor.enqueued_through_snapshot_line >= 0 || cursor.enqueued_through_message_id.is_some();
                if !has_cursor
                    && let Some((end_msg_id, end_line)) = resolve_boundary_to_canonical(&entries, prov)
                    && let Err(error) = self.queue.seed_enqueued_through(conversation, &end_msg_id, end_line)
                {
                    (self.options.warn)(&format!("facts queue seed failed: {conversation}: {error}"));
                }
            }

            self.queue.enqueue(FactsEnqueueRequest { identity:self.options.identity.clone(), session_id:conversation.into(), conversation_id:conversation.into(), entries, signal:signal.cloned() }).map_err(|error| error.to_string())
        };
        match enqueue() { Ok(result) => result, Err(error) => { (self.options.warn)(&format!("facts queue enqueue failed: {conversation}: {error}")); disabled() } }
    }
    pub fn on_settled(&mut self, conversation: &str) -> FactsEnqueueResult {
        let pending = if self.settled_sessions.contains(conversation) { Vec::new() } else { self.reconcile_pending() };
        let result = self.enqueue_settled(conversation, None);
        if !(self.options.facts_enabled)() { return result; }
        self.settled_sessions.insert(conversation.to_owned());
        self.settles += 1;
        if !pending.is_empty() || self.settles >= (self.options.debounce_settles)().max(1) {
            self.settles = 0;
            self.fire(false);
        }
        result
    }
    pub fn reconcile_pending(&self) -> Vec<FactsQueueEntry> {
        if !(self.options.facts_enabled)() { return vec![]; }
        match self.queue.list_pending_files() { Ok(files) => files.into_iter().map(|file| file.header).collect(), Err(error) => { (self.options.warn)(&format!("facts queue reconcile failed: {error}")); vec![] } }
    }
    pub fn reconcile_extractor(&mut self) { self.reconcile_extractor_for(None); }
    pub fn reconcile_extractor_for(&mut self, conversation: Option<&str>) {
        if let Some(conversation) = conversation { self.settled_sessions.remove(conversation); }
        if !(self.options.facts_enabled)() { return; }
        self.fire(true);
    }
    pub fn launch_extractor(&mut self) { if (self.options.facts_enabled)() { self.fire(false); } }
    pub async fn cancel_active(&mut self) { if let Some(extractor)=&mut self.options.extractor && let Err(error)=extractor.cancel_active().await { maho_ext_api::diagnostic(&format!("memory facts cancel failed: {error}")); } }
    pub async fn launch_if_threshold_met(&mut self, signal: Option<&FactsAbortSignal>) -> bool {
        if signal.is_some_and(FactsAbortSignal::is_aborted) || !(self.options.facts_enabled)() || self.settles < (self.options.debounce_settles)().max(1) { return false; }
        if signal.is_some_and(FactsAbortSignal::is_aborted) { return false; }
        let Some(extractor) = &mut self.options.extractor else { return false; };
        self.settles = 0;
        if signal.is_some_and(FactsAbortSignal::is_aborted) { return false; }
        if let Err(error) = extractor.launch_pending(signal).await { (self.options.warn)(&format!("facts extractor launch failed: {error}")); }
        true
    }
    pub async fn launch_pending_now(&mut self, signal: Option<&FactsAbortSignal>) -> bool {
        if signal.is_some_and(FactsAbortSignal::is_aborted) || !(self.options.facts_enabled)() { return false; }
        self.settles = 0;
        let Some(extractor) = &mut self.options.extractor else { return false; };
        if let Err(error) = extractor.launch_pending(signal).await { (self.options.warn)(&format!("facts extractor launch failed: {error}")); }
        true
    }
    pub fn mark_consumed(&self, entries: &[FactsQueueEntry]) {
        if let Err(error) = self.queue.mark_consumed(entries) { (self.options.warn)(&format!("facts queue consume failed: {error}")); }
    }
}

pub fn resolve_boundary_to_canonical(
    entries: &[memory_core::journal::TranscriptEntry],
    provenance: &maho_ext_api::ForkProvenance,
) -> Option<(String, u64)> {
    let boundary_id = provenance.inherited_through_entry_id.as_deref()?;
    let mut last_canonical: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if memory_core::journal::is_canonical_entry(entry) {
            last_canonical.insert(entry.source_message_id(), index);
        }
    }
    if let Some(&idx) = last_canonical.get(boundary_id) {
        return Some((entries[idx].source_message_id().to_string(), (idx + 1) as u64));
    }
    let parent_path = std::path::Path::new(&provenance.parent_session);
    if let Ok(file) = std::fs::File::open(parent_path) {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(file);
        let mut preceding_message_id: Option<String> = None;
        for line_result in reader.lines() {
            let Ok(line) = line_result else { continue };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if val.get("type").and_then(serde_json::Value::as_str) == Some("session") { continue; }
            let id = val.get("id").and_then(serde_json::Value::as_str);
            if val.get("type").and_then(serde_json::Value::as_str) == Some("message")
                && let Some(msg_id) = id
                && last_canonical.contains_key(msg_id)
            {
                preceding_message_id = Some(msg_id.to_string());
            }
            if id == Some(boundary_id) {
                break;
            }
        }
        if let Some(msg_id) = preceding_message_id
            && let Some(&idx) = last_canonical.get(msg_id.as_str())
        {
            return Some((entries[idx].source_message_id().to_string(), (idx + 1) as u64));
        }
    }
    None
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
    struct ThreadProbe(Option<tokio::sync::oneshot::Sender<std::thread::ThreadId>>);
    impl FactsExtractorPort for ThreadProbe {
        fn launch_pending(&mut self, _: Option<&FactsAbortSignal>) -> FactsExtractorWork {
            let report = self.0.take().unwrap();
            Box::pin(async move { report.send(std::thread::current().id()).map_err(|_| "probe receiver dropped".to_owned()) })
        }
        fn reconcile_pending(&mut self, signal: Option<&FactsAbortSignal>) -> FactsExtractorWork { self.launch_pending(signal) }
    }
    #[tokio::test]
    async fn reconcile_work_runs_off_the_calling_thread() {
        let root = tempfile::tempdir().unwrap();
        let (mut wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let (report, reported) = tokio::sync::oneshot::channel();
        wiring.options.extractor = Some(Box::new(ThreadProbe(Some(report))));
        wiring.reconcile_extractor();
        let worker = tokio::time::timeout(std::time::Duration::from_secs(5), reported).await.unwrap().unwrap();
        assert_ne!(worker, std::thread::current().id());
    }
    #[tokio::test] async fn debounce_counts_settles_even_without_new_entries() { let root=tempfile::tempdir().unwrap(); let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(4))); for _ in 0..3 { assert!(!wiring.on_settled("conversation").is_enqueued()); } assert_eq!(calls.get(),0); wiring.on_settled("conversation"); assert_eq!(calls.get(),1); }
    #[tokio::test] async fn repeated_session_admission_is_not_reconciled_again(){let root=tempfile::tempdir().unwrap();let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(4)));wiring.on_settled("same");assert_eq!(calls.get(),0);wiring.on_settled("same");assert_eq!(calls.get(),0);}
    #[tokio::test] async fn existing_pending_work_bypasses_debounce(){let root=tempfile::tempdir().unwrap();let (mut wiring,calls)=fixture(root.path(),true,Rc::new(Cell::new(99)));let journal=TranscriptJournal::new(TranscriptJournalOptions::new(wiring.options.identity_paths.transcripts.join("old")));journal.append(&[memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("user","q","2026-01-01T00:00:00Z","u","m")),memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("assistant","a","2026-01-01T00:00:00Z","a","m"))]).unwrap();assert!(wiring.enqueue_settled("old",None).is_enqueued());wiring.on_settled("new");assert_eq!(calls.get(),1);}
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

    fn text_entry(role: &str, msg_id: &str, text: &str) -> memory_core::journal::TranscriptEntry {
        memory_core::journal::TranscriptEntry::Text(
            memory_core::journal::TextTranscriptEntry::new(
                role,
                text,
                "2026-01-01T00:00:00.000Z",
                format!("{msg_id}:{role}"),
                msg_id,
            ),
        )
    }

    #[test]
    fn test_fork_with_3_inherited_and_1_new_message_enqueues_only_the_new_message() {
        let root = tempfile::tempdir().unwrap();
        let (wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
            wiring.options.identity_paths.transcripts.join("fork-session"),
        ));
        journal
            .append(&[
                text_entry("user", "m1", "First"),
                text_entry("assistant", "m2", "Second"),
                text_entry("user", "m3", "Third"),
                text_entry("assistant", "m4", "Fourth"),
            ])
            .unwrap();

        let prov = maho_ext_api::ForkProvenance {
            parent_session: "/dummy/parent/session.jsonl".to_string(),
            inherited_through_entry_id: Some("m3".to_string()),
        };
        wiring.set_provenance("fork-session", Some(prov));

        let result = wiring.enqueue_settled("fork-session", None);
        let entry = match result {
            FactsEnqueueResult::Enqueued { entry } => entry,
            other => panic!("expected enqueued, got {other:?}"),
        };
        assert_eq!(entry.range.start_message_id, "m4");
        assert_eq!(entry.range.end_message_id, "m4");
        assert_eq!(entry.range.start_line, 3);
        assert_eq!(entry.entries.len(), 1);
        assert_eq!(entry.entries[0].source_message_id(), "m4");
    }

    #[test]
    fn test_shutdown_before_first_settle_on_a_fork_seeds_the_same_way() {
        let root = tempfile::tempdir().unwrap();
        let (wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
            wiring.options.identity_paths.transcripts.join("shutdown-fork"),
        ));
        journal
            .append(&[
                text_entry("user", "m1", "First"),
                text_entry("assistant", "m2", "Second"),
                text_entry("user", "m3", "Third"),
                text_entry("assistant", "m4", "Fourth"),
            ])
            .unwrap();

        let prov = maho_ext_api::ForkProvenance {
            parent_session: "/dummy/parent/session.jsonl".to_string(),
            inherited_through_entry_id: Some("m3".to_string()),
        };
        // Provenance captured during shutdown before first settle
        wiring.set_provenance("shutdown-fork", Some(prov));

        let result = wiring.enqueue_settled("shutdown-fork", None);
        let entry = match result {
            FactsEnqueueResult::Enqueued { entry } => entry,
            other => panic!("expected enqueued, got {other:?}"),
        };
        assert_eq!(entry.range.start_message_id, "m4");
        assert_eq!(entry.range.end_message_id, "m4");
        assert_eq!(entry.range.start_line, 3);
        assert_eq!(entry.entries.len(), 1);
    }

    #[test]
    fn test_non_fork_still_enqueues_from_line_0() {
        let root = tempfile::tempdir().unwrap();
        let (wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
            wiring.options.identity_paths.transcripts.join("non-fork"),
        ));
        journal
            .append(&[
                text_entry("user", "m1", "First"),
                text_entry("assistant", "m2", "Second"),
                text_entry("user", "m3", "Third"),
            ])
            .unwrap();

        // No provenance set
        let result = wiring.enqueue_settled("non-fork", None);
        let entry = match result {
            FactsEnqueueResult::Enqueued { entry } => entry,
            other => panic!("expected enqueued, got {other:?}"),
        };
        assert_eq!(entry.range.start_line, 0);
        assert_eq!(entry.range.start_message_id, "m1");
        assert_eq!(entry.range.end_message_id, "m3");
        assert_eq!(entry.entries.len(), 3);
    }

    #[test]
    fn test_old_style_fork_with_unreadable_parent_not_seeded() {
        let root = tempfile::tempdir().unwrap();
        let (wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
            wiring.options.identity_paths.transcripts.join("unreadable-parent"),
        ));
        journal
            .append(&[
                text_entry("user", "m1", "First"),
                text_entry("assistant", "m2", "Second"),
                text_entry("user", "m3", "Third"),
                text_entry("assistant", "m4", "Fourth"),
            ])
            .unwrap();

        // Unreadable parent with no inherited boundary id
        let prov = maho_ext_api::ForkProvenance {
            parent_session: "/nonexistent/unreadable/parent.jsonl".to_string(),
            inherited_through_entry_id: None,
        };
        wiring.set_provenance("unreadable-parent", Some(prov));

        let result = wiring.enqueue_settled("unreadable-parent", None);
        let entry = match result {
            FactsEnqueueResult::Enqueued { entry } => entry,
            other => panic!("expected enqueued, got {other:?}"),
        };
        // Not seeded -> starts from line 0
        assert_eq!(entry.range.start_line, 0);
        assert_eq!(entry.range.start_message_id, "m1");
        assert_eq!(entry.range.end_message_id, "m4");
        assert_eq!(entry.entries.len(), 4);
    }

    #[test]
    fn test_seed_enqueued_through_failure_reports_warning() {
        let root = tempfile::tempdir().unwrap();
        let (mut wiring, _) = fixture(root.path(), true, Rc::new(Cell::new(4)));
        let (warn_tx, warn_rx) = std::sync::mpsc::channel();
        wiring.options.warn = Arc::new(move |msg| {
            let _ = warn_tx.send(msg.to_string());
        });
        let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
            wiring.options.identity_paths.transcripts.join("seed-fail"),
        ));
        journal
            .append(&[
                text_entry("user", "m1", "First"),
                text_entry("assistant", "m2", "Second"),
            ])
            .unwrap();

        // Make the temporary cursor path a directory so write_cursor fails deterministically while reading cursor succeeds
        let queue_paths = memory_core::facts::schema::facts_queue_paths(&wiring.options.identity_paths);
        std::fs::create_dir_all(&queue_paths.cursor_dir).unwrap();
        let cursor_path = queue_paths.cursor_path("seed-fail");
        let tmp_path = std::path::PathBuf::from(format!("{}.tmp", cursor_path.display()));
        std::fs::create_dir_all(&tmp_path).unwrap();

        let prov = maho_ext_api::ForkProvenance {
            parent_session: "/dummy/parent/session.jsonl".to_string(),
            inherited_through_entry_id: Some("m1".to_string()),
        };
        wiring.set_provenance("seed-fail", Some(prov));

        let _ = wiring.enqueue_settled("seed-fail", None);

        let warnings: Vec<String> = warn_rx.try_iter().collect();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("facts queue seed failed: seed-fail")),
            "expected seed warning, got {warnings:?}"
        );
    }

    #[test]
    fn test_resolve_boundary_to_canonical_streams_parent_fallback() {
        let root = tempfile::tempdir().unwrap();
        let parent_file = root.path().join("parent.jsonl");
        let parent_content = [
            r#"{"type":"session","id":"sess-1"}"#,
            r#"{"type":"message","id":"m1","message":{"role":"user","content":"first"}}"#,
            r#"{"type":"thought","id":"t1","content":"thinking"}"#,
            r#"{"type":"message","id":"m2","message":{"role":"assistant","content":"second"}}"#,
            r#"{"type":"tool_call","id":"tc1","tool":"test"}"#,
        ].join("\n");
        std::fs::write(&parent_file, parent_content).unwrap();

        let entries = vec![
            text_entry("user", "m1", "first"),
            text_entry("assistant", "m2", "second"),
        ];

        let prov = maho_ext_api::ForkProvenance {
            parent_session: parent_file.to_str().unwrap().to_string(),
            inherited_through_entry_id: Some("tc1".to_string()),
        };
        let resolved = resolve_boundary_to_canonical(&entries, &prov);
        assert_eq!(resolved, Some(("m2".to_string(), 2)));
    }
}
