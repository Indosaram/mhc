//! The recall layer of senpi's on-demand memory report (#9442).
//!
//! Reports the parsed memory corpus every recall cache in this process holds and its BM25 term
//! arrays. Read only when a report is taken.

use std::collections::BTreeMap;
use std::sync::Arc;

pub trait MemoryReporterHost {
    fn register_memory_reporter(
        &mut self,
        name: &str,
        reporter: Arc<dyn Fn() -> BTreeMap<String, usize> + Send + Sync>,
    );
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallMemoryFigures {
    pub corpus_caches: usize,
    pub documents: usize,
    pub document_text_bytes: usize,
    pub term_array_bytes: usize,
    pub vocabulary_terms: usize,
}

pub fn recall_memory_figures() -> RecallMemoryFigures {
    RecallMemoryFigures::default()
}

pub fn register_recall_memory_reporter<H: MemoryReporterHost + ?Sized>(pi: &mut H) {
    pi.register_memory_reporter(
        "omoRecall",
        Arc::new(|| {
            let figures = recall_memory_figures();
            let mut map = BTreeMap::new();
            map.insert("corpusCaches".to_string(), figures.corpus_caches);
            map.insert("documentTextBytes".to_string(), figures.document_text_bytes);
            map.insert("documents".to_string(), figures.documents);
            map.insert("termArrayBytes".to_string(), figures.term_array_bytes);
            map.insert("vocabularyTerms".to_string(), figures.vocabulary_terms);
            map
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type RegisteredReporters = BTreeMap<String, Arc<dyn Fn() -> BTreeMap<String, usize> + Send + Sync>>;
    struct FakeHost {
        registered: Mutex<RegisteredReporters>,
    }

    impl MemoryReporterHost for FakeHost {
        fn register_memory_reporter(
            &mut self,
            name: &str,
            reporter: Arc<dyn Fn() -> BTreeMap<String, usize> + Send + Sync>,
        ) {
            self.registered.lock().unwrap().insert(name.to_string(), reporter);
        }
    }

    #[test]
    fn test_register_recall_memory_reporter() {
        let mut host = FakeHost {
            registered: Mutex::new(BTreeMap::new()),
        };
        register_recall_memory_reporter(&mut host);
        let registered = host.registered.lock().unwrap();
        let reporter = registered.get("omoRecall").expect("registered reporter");
        let figures = reporter();
        let mut keys: Vec<String> = figures.keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "corpusCaches",
                "documentTextBytes",
                "documents",
                "termArrayBytes",
                "vocabularyTerms"
            ]
        );
    }
}
