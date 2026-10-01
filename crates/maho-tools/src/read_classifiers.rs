use std::{path::Path, sync::{Arc, Mutex, OnceLock}, sync::atomic::{AtomicU64, Ordering}};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactReadKind { Docs, Resource, Skill, Memory }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactReadClassification { pub kind: CompactReadKind, pub label: String, pub headline: Option<String> }
pub type ReadClassifier = Arc<dyn Fn(&Path, &Path) -> Result<Option<CompactReadClassification>, String> + Send + Sync>;
type Classifiers = Vec<(u64,ReadClassifier)>;
static CLASSIFIERS: OnceLock<Mutex<Classifiers>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct ReadClassifierRegistration(u64);
impl Drop for ReadClassifierRegistration {
    fn drop(&mut self) { match CLASSIFIERS.get_or_init(|| Mutex::new(Vec::new())).lock() { Ok(mut list) => list.retain(|(id,_)| *id != self.0), Err(error) => eprintln!("Read classifier unregister failed: {error}") } }
}
pub fn register_read_classifier(classifier: ReadClassifier) -> Result<ReadClassifierRegistration, String> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    CLASSIFIERS.get_or_init(|| Mutex::new(Vec::new())).lock().map_err(|e| e.to_string())?.push((id,classifier)); Ok(ReadClassifierRegistration(id))
}
pub fn classify_read(path: &Path, cwd: &Path) -> Option<CompactReadClassification> {
    let classifiers = match CLASSIFIERS.get_or_init(|| Mutex::new(Vec::new())).lock() { Ok(list) => list.clone(), Err(error) => { eprintln!("Read classifier failed: {error}"); return None; } };
    for (_, classifier) in classifiers {
        match classifier(path,cwd) { Ok(Some(classification)) => return Some(classification), Ok(None) => {}, Err(error) => eprintln!("Read classifier failed: {error}") }
    } None
}
