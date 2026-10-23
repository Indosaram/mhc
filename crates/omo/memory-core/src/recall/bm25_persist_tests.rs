use std::sync::{Arc, Mutex};

use super::{
    RECALL_TERM_STORE_NAME, RecallTermPersistence, tokenizer_fingerprint,
    with_recall_term_saves_excluded,
};
use crate::recall::bm25::document_key;
use crate::recall::bm25::rank_recall_documents_bm25;
use crate::recall::bm25_store::decode_recall_term_store;
use crate::recall::corpus_derived::{register, unregister};
use crate::recall::provider::{RecallCorpus, RecallDocument};

fn corpus(tag: &str) -> Arc<RecallCorpus> {
    let document = |name: &str, body: &str| RecallDocument {
        path: format!("{name}.md"),
        description: format!("persist {tag} {name}"),
        body: body.to_string(),
    };
    Arc::new(RecallCorpus {
        revision: Some(tag.to_string()),
        documents: vec![
            document("a", &format!("{tag}alpha {tag}beta rollback checklists")),
            document("b", &format!("{tag}beta only 배포절차")),
        ],
    })
}

fn rank(corpus: &RecallCorpus) {
    let _ = rank_recall_documents_bm25(&corpus.documents, &["rollback".to_string()], None);
}

fn memory_dir(git_is_dir: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    if git_is_dir {
        std::fs::create_dir(dir.path().join(".git")).expect("git dir");
    } else {
        std::fs::write(dir.path().join(".git"), "gitdir: elsewhere\n").expect("git file");
    }
    dir
}

fn persistence(dir: &std::path::Path) -> (RecallTermPersistence, Arc<Mutex<Vec<String>>>) {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let persistence = RecallTermPersistence::new(
        dir,
        Arc::new(move |error| {
            if let Ok(mut errors) = sink.lock() {
                errors.push(error.to_string());
            }
        }),
    );
    (persistence, errors)
}

#[test]
fn a_build_that_tokenized_saves_a_decodable_store() {
    let dir = memory_dir(true);
    let corpus = corpus("persistsave");
    register(&corpus);
    let (persistence, errors) = persistence(dir.path());
    persistence.attach(&corpus);
    rank(&corpus);
    persistence.settled();

    let bytes =
        std::fs::read(dir.path().join(".git").join(RECALL_TERM_STORE_NAME)).expect("store written");
    let store = decode_recall_term_store(&bytes).expect("store decodes");
    assert_eq!(store.fingerprint, tokenizer_fingerprint());
    for document in &corpus.documents {
        assert!(store.documents.contains_key(&document_key(document)));
    }
    let leftovers: Vec<String> = std::fs::read_dir(dir.path().join(".git"))
        .expect("read git dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    assert!(errors.lock().expect("errors").is_empty());
    unregister(&corpus);
}

#[test]
fn a_git_file_instead_of_a_directory_persists_nothing() {
    let dir = memory_dir(false);
    let corpus = corpus("persistnogit");
    register(&corpus);
    let (persistence, errors) = persistence(dir.path());
    persistence.attach(&corpus);
    rank(&corpus);
    persistence.settled();
    assert!(
        !dir.path()
            .join(".git")
            .join(RECALL_TERM_STORE_NAME)
            .exists()
    );
    assert!(errors.lock().expect("errors").is_empty());
    unregister(&corpus);
}

#[test]
fn saves_requested_while_excluded_are_dropped() {
    let dir = memory_dir(true);
    let corpus = corpus("persistexcluded");
    register(&corpus);
    let (persistence, _errors) = persistence(dir.path());
    persistence.attach(&corpus);
    with_recall_term_saves_excluded(dir.path(), || rank(&corpus));
    persistence.settled();
    assert!(
        !dir.path()
            .join(".git")
            .join(RECALL_TERM_STORE_NAME)
            .exists()
    );
    unregister(&corpus);
}

#[test]
fn a_corrupt_store_reads_as_empty_without_error() {
    let dir = memory_dir(true);
    std::fs::write(
        dir.path().join(".git").join(RECALL_TERM_STORE_NAME),
        b"not a store",
    )
    .expect("write");
    let corpus = corpus("persistcorrupt");
    register(&corpus);
    let (persistence, errors) = persistence(dir.path());
    persistence.attach(&corpus);
    rank(&corpus);
    persistence.settled();
    let bytes =
        std::fs::read(dir.path().join(".git").join(RECALL_TERM_STORE_NAME)).expect("rewritten");
    assert!(decode_recall_term_store(&bytes).is_some());
    assert!(errors.lock().expect("errors").is_empty());
    unregister(&corpus);
}
