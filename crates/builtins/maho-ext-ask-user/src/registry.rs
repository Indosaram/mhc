use maho_ext_api::{ExtensionApi, ExtensionContext, QuestionRequest, QuestionResponse, QuestionStatus};
use std::{collections::BTreeMap, sync::{Arc, Mutex, OnceLock}};
use tokio::sync::watch;

pub struct PendingQuestionEntry {
    pub request: QuestionRequest,
    pub completion: watch::Receiver<Option<QuestionResponse>>,
    pub cancel: Arc<dyn Fn(QuestionStatus) + Send + Sync>,
    pub owner: watch::Sender<Option<QuestionOwner>>,
    pub publication: Arc<Mutex<watch::Sender<bool>>>,
    pub deadline_at_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}
#[derive(Clone)]
pub struct QuestionOwner {
    pub sender: Arc<ExtensionApi>,
    pub context: ExtensionContext,
    pub state: Arc<Mutex<crate::tool::AskUserState>>,
}
pub type QueuedOutcome = Box<dyn FnOnce(QuestionOwner) + Send>;
pub(crate) fn select_publication<T: Clone>(owners: &watch::Receiver<Option<T>>, publication: &Mutex<watch::Sender<bool>>, queue: impl FnOnce()) -> Option<T> {
    let publishing = publication.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let owner = owners.borrow().clone();
    publishing.send_replace(true);
    if owner.is_none() { queue(); }
    owner
}
fn outcomes() -> &'static Mutex<BTreeMap<String, Vec<QueuedOutcome>>> {
    static OUTCOMES: OnceLock<Mutex<BTreeMap<String, Vec<QueuedOutcome>>>> = OnceLock::new();
    OUTCOMES.get_or_init(Mutex::default)
}
pub fn queue_outcome(session: &str, outcome: QueuedOutcome) {
    outcomes().lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(session.into()).or_default().push(outcome);
}
pub fn deliver_outcomes(session: &str, owner: QuestionOwner) {
    let queued = outcomes().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session).unwrap_or_default();
    for outcome in queued { outcome(owner.clone()); }
}
type Sessions = BTreeMap<String, Vec<Arc<PendingQuestionEntry>>>;
fn sessions() -> &'static Mutex<Sessions> {
    static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
    SESSIONS.get_or_init(Mutex::default)
}
pub fn get_pending_questions(session: &str) -> Vec<Arc<PendingQuestionEntry>> {
    sessions().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session).cloned().unwrap_or_default()
}
pub fn register_pending_question(session: &str, entry: Arc<PendingQuestionEntry>) {
    let mut sessions=sessions().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entries=sessions.entry(session.into()).or_default();
    if let Some(existing)=entries.iter_mut().find(|existing|existing.request.request_id==entry.request.request_id){*existing=entry;}else{entries.push(entry);}
}
pub fn unregister_pending_question(session: &str, request: &str) {
    let mut sessions = sessions().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entries) = sessions.get_mut(session) {
        entries.retain(|entry|entry.request.request_id!=request);
        if entries.is_empty() { sessions.remove(session); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_selection_preserves_insertion_order_not_request_id_order(){
        let entry=|id:&str|{
            let (_,completion)=watch::channel(None);
            let (owner,_)=watch::channel(None);
            let (publication,_)=watch::channel(false);
            Arc::new(PendingQuestionEntry{request:QuestionRequest{request_id:id.into(),questions:vec![],wait_for_answer:false,timeout_ms:100},completion,cancel:Arc::new(|_|{}),owner,publication:Arc::new(Mutex::new(publication)),deadline_at_ms:Arc::new(||100)})
        };
        let session="registry-insertion-order";
        register_pending_question(session,entry("z-first"));register_pending_question(session,entry("a-second"));
        let initial=get_pending_questions(session);
        let replacement=entry("z-first");register_pending_question(session,replacement.clone());
        let replaced=get_pending_questions(session);
        unregister_pending_question(session,"z-first");let remaining=get_pending_questions(session);
        unregister_pending_question(session,"a-second");let empty=get_pending_questions(session).is_empty();
        assert_eq!(initial.iter().map(|entry|entry.request.request_id.as_str()).collect::<Vec<_>>(),["z-first","a-second"]);
        assert!(Arc::ptr_eq(&replaced[0],&replacement));assert_eq!(replaced[1].request.request_id,"a-second");
        assert_eq!(remaining[0].request.request_id,"a-second");assert!(empty);
    }
    #[test]
    fn detached_publication_enqueues_before_rebind_can_drain() {
        let (owners, receiver) = watch::channel(None::<u8>);
        let (publishing, _) = watch::channel(false);
        let publication = Arc::new(Mutex::new(publishing));
        let queue = Arc::new(Mutex::new(Vec::new()));
        let (captured, captured_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let work_lock = publication.clone();
        let work_queue = queue.clone();
        let worker = std::thread::spawn(move || select_publication(&receiver, &work_lock, || {
            captured.send(()).expect("captured detached owner");
            release_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("release publication");
            work_queue.lock().expect("queue").push(1);
        }));
        captured_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("capture barrier");
        let protected = publication.try_lock().is_err();
        let deliveries = if protected {
            release.send(()).expect("release");
            let guard = publication.lock().expect("publication");
            owners.send_replace(Some(7));
            let deliveries = std::mem::take(&mut *queue.lock().expect("queue"));
            drop(guard);
            deliveries
        } else {
            let guard = publication.lock().expect("publication");
            owners.send_replace(Some(7));
            let deliveries = std::mem::take(&mut *queue.lock().expect("queue"));
            drop(guard);
            release.send(()).expect("release");
            deliveries
        };
        assert!(worker.join().expect("worker").is_none());
        let stranded = std::mem::take(&mut *queue.lock().expect("queue"));
        assert!(protected, "SessionStart can drain before detached outcome enqueue: delivered={deliveries:?}, stranded={stranded:?}");
        assert_eq!(deliveries, [1]);
        assert!(stranded.is_empty());
    }
}
