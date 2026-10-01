//! Port of senpi packages/agent/test/harness/jsonl-io.test.ts.

mod support;

use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::session::jsonl::io::{AppendFn, TxAppendFn, publish_file_atomically, publish_jsonl};
use maho_agent::harness::session::jsonl::types::{JsonlStorageHeader, JSONL_STORAGE_VERSION};
use maho_agent::harness::session::values::{set_value, session_name};
use maho_agent::harness::types::{FileError, FileErrorCode, FileSystem};

fn temp_root(label: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "maho-jsonl-io-{label}-{}-{}",
        std::process::id(),
        support::unique_suffix()
    ));
    std::fs::create_dir_all(&path).expect("temp root");
    path.to_string_lossy().into_owned()
}

fn header() -> JsonlStorageHeader {
    let mut header = JsonlStorageHeader::new("session", JSONL_STORAGE_VERSION, 1_700_000_000_000, "/workspace");
    header.next_seq = Some(4);
    header
}

#[tokio::test]
async fn keeps_the_destination_unchanged_until_all_content_has_been_written() {
    let root = temp_root("atomic");
    let path = format!("{root}/session.jsonl");
    std::fs::write(&path, "original").expect("seed");
    let file_system: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::new(root.clone()));

    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    publish_file_atomically(
        file_system.clone(),
        path.clone(),
        &BACKGROUND_CONTEXT,
        Arc::new({
            let observed = observed.clone();
            let path = path.clone();
            move |append: AppendFn| {
                let observed = observed.clone();
                let path = path.clone();
                Box::pin(async move {
                    append("first\n".to_owned()).await?;
                    observed
                        .lock()
                        .expect("observed")
                        .push(std::fs::read_to_string(format!("{path}.tmp")).expect("temp"));
                    observed
                        .lock()
                        .expect("observed")
                        .push(std::fs::read_to_string(&path).expect("destination"));
                    append("second\n".to_owned()).await?;
                    observed
                        .lock()
                        .expect("observed")
                        .push(std::fs::read_to_string(&path).expect("destination"));
                    Ok(())
                })
            }
        }),
    )
    .await
    .expect("publish");

    assert_eq!(std::fs::read_to_string(&path).expect("destination"), "first\nsecond\n");
    assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
    let observed = observed.lock().expect("observed").clone();
    assert_eq!(observed, vec!["first\n".to_owned(), "original".to_owned(), "original".to_owned()]);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn discards_partial_content_and_preserves_the_original_error_when_the_callback_fails() {
    let root = temp_root("failure");
    let path = format!("{root}/session.jsonl");
    std::fs::write(&path, "original").expect("seed");
    let file_system: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::new(root.clone()));

    let error = publish_file_atomically(
        file_system.clone(),
        path.clone(),
        &BACKGROUND_CONTEXT,
        Arc::new(|append: AppendFn| {
            Box::pin(async move {
                append("partial".to_owned()).await?;
                Err(maho_agent::harness::session::SessionError::io("content generation failed"))
            })
        }),
    )
    .await
    .expect_err("failure");
    assert_eq!(error.message, "content generation failed");

    assert_eq!(std::fs::read_to_string(&path).expect("destination"), "original");
    assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn preserves_the_destination_and_allows_retry_after_a_failing_append() {
    let root = temp_root("retry");
    let path = format!("{root}/session.jsonl");
    std::fs::write(&path, "original").expect("seed");
    let inner: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::new(root.clone()));
    let file_system: Arc<dyn FileSystem> = Arc::new(support::ObservingFileSystem::new(inner, 1));

    let error = publish_file_atomically(
        file_system.clone(),
        path.clone(),
        &BACKGROUND_CONTEXT,
        Arc::new(|append: AppendFn| Box::pin(async move { append("replacement".to_owned()).await })),
    )
    .await
    .expect_err("injected append failure");
    assert!(error.message.contains("injected I/O failure"), "{}", error.message);

    assert_eq!(std::fs::read_to_string(&path).expect("destination"), "original");
    assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());

    publish_file_atomically(
        file_system.clone(),
        path.clone(),
        &BACKGROUND_CONTEXT,
        Arc::new(|append: AppendFn| Box::pin(async move { append("retry".to_owned()).await })),
    )
    .await
    .expect("retry");
    assert_eq!(std::fs::read_to_string(&path).expect("destination"), "retry");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn writes_the_header_first_and_preserves_transaction_boundaries() {
    let root = temp_root("publication");
    let path = format!("{root}/session.jsonl");
    let file_system: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::new(root.clone()));
    let header = header();

    let first = serde_json::json!({ "kind": "value", "op": "set", "seq": 1, "namespace": "pi.session.name", "key": "", "value": "first" });
    let second = serde_json::json!({ "kind": "value", "op": "set", "seq": 2, "namespace": "pi.session.name", "key": "", "value": "second" });
    let third = serde_json::json!({ "kind": "value", "op": "set", "seq": 3, "namespace": "pi.session.name", "key": "", "value": "third" });

    let transactions: Vec<Vec<serde_json::Value>> = vec![vec![first.clone()], vec![second.clone(), third.clone()]];
    publish_jsonl(
        file_system.clone(),
        path.clone(),
        &header,
        &BACKGROUND_CONTEXT,
        Arc::new(move |append: TxAppendFn| {
            let transactions = transactions.clone();
            Box::pin(async move {
                for transaction in transactions {
                    let writes = transaction
                        .iter()
                        .map(|value| {
                            maho_agent::harness::session::jsonl::io::parse_committed_write(value).expect("write")
                        })
                        .collect();
                    append(writes).await?;
                }
                Ok(())
            })
        }),
    )
    .await
    .expect("publish");

    let expected = [
        serde_json::to_string(&header).expect("header"),
        first.to_string(),
        serde_json::Value::Array(vec![second, third]).to_string(),
        String::new(),
    ]
    .join("\n");
    assert_eq!(std::fs::read_to_string(&path).expect("destination"), expected);
    std::fs::remove_dir_all(&root).expect("cleanup");

    let _ = set_value(&session_name(), serde_json::json!("unused"));
    let _ = FileError::new(FileErrorCode::Unknown, "unused", None);
}

#[tokio::test]
async fn publishes_a_header_only_file_when_the_callback_emits_no_transactions() {
    let root = temp_root("header-only");
    let path = format!("{root}/session.jsonl");
    let file_system: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::new(root.clone()));
    let header = header();

    publish_jsonl(
        file_system.clone(),
        path.clone(),
        &header,
        &BACKGROUND_CONTEXT,
        Arc::new(|_append: TxAppendFn| Box::pin(async move { Ok(()) })),
    )
    .await
    .expect("publish");
    assert_eq!(
        std::fs::read_to_string(&path).expect("destination"),
        format!("{}\n", serde_json::to_string(&header).expect("header"))
    );
    std::fs::remove_dir_all(&root).expect("cleanup");
}
