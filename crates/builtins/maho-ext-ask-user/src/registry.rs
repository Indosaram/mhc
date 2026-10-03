use maho_ext_api::{ExtensionApi, ExtensionContext, QuestionRequest, QuestionResponse, QuestionStatus};
use std::{collections::BTreeMap, sync::{Arc, Mutex, OnceLock}};
use tokio::sync::watch;

pub struct PendingQuestionEntry {
    pub request: QuestionRequest,
    pub completion: watch::Receiver<Option<QuestionResponse>>,
    pub cancel: Arc<dyn Fn(QuestionStatus) + Send + Sync>,
    pub owner: watch::Sender<Option<QuestionOwner>>,
    pub publication: Arc<Mutex<watch::Sender<bool>>>,
}
#[derive(Clone)]
pub struct QuestionOwner {
    pub sender: Arc<ExtensionApi>,
    pub context: ExtensionContext,
    pub state: Arc<Mutex<crate::tool::AskUserState>>,
}
pub type QueuedOutcome = Box<dyn FnOnce(QuestionOwner) + Send>;
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
type Sessions = BTreeMap<String, BTreeMap<String, Arc<PendingQuestionEntry>>>;
fn sessions() -> &'static Mutex<Sessions> {
    static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
    SESSIONS.get_or_init(Mutex::default)
}
pub fn get_pending_questions(session: &str) -> Vec<Arc<PendingQuestionEntry>> {
    sessions().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session).map_or_else(Vec::new, |entries| entries.values().cloned().collect())
}
pub fn register_pending_question(session: &str, entry: Arc<PendingQuestionEntry>) {
    sessions().lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(session.into()).or_default().insert(entry.request.request_id.clone(), entry);
}
pub fn unregister_pending_question(session: &str, request: &str) {
    let mut sessions = sessions().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entries) = sessions.get_mut(session) {
        entries.remove(request);
        if entries.is_empty() { sessions.remove(session); }
    }
}
