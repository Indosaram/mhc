//! Port of senpi packages/agent/test/harness/jsonl-session-repo.test.ts.

mod support;

use std::sync::Arc;

use maho_agent::harness::context::{BACKGROUND_CONTEXT, Context};
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::session::jsonl::types::{JSONL_STORAGE_VERSION, JsonlSessionListOptions};
use maho_agent::harness::session::jsonl::{JsonlSessionCreateOptions, JsonlSessionRepo, JsonlSessionRepoOptions};
use maho_agent::harness::session::types::{ForkOptions, Write};
use maho_agent::harness::session::values::{session_name, set_value};
use maho_agent::harness::types::FileSystem;

const NOW: i64 = 1_700_000_000_000;

fn temp_root(label: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "maho-jsonl-repo-{label}-{}-{}",
        std::process::id(),
        support::unique_suffix()
    ));
    std::fs::create_dir_all(&path).expect("temp root");
    path.to_string_lossy().into_owned()
}

fn node_env(root: &str) -> Arc<dyn FileSystem> {
    Arc::new(NodeExecutionEnv::new(root.to_owned()))
}

fn repo(file_system: Arc<dyn FileSystem>) -> JsonlSessionRepo {
    JsonlSessionRepo::new(JsonlSessionRepoOptions {
        file_system,
        sessions_root: "sessions".to_owned(),
        now: Some(Arc::new(|| NOW)),
    })
}

fn create_options(id: &str, cwd: &str) -> JsonlSessionCreateOptions {
    JsonlSessionCreateOptions {
        id: Some(id.to_owned()),
        parent_session_id: None,
        cwd: cwd.to_owned(),
    }
}

async fn only_metadata(repo: &JsonlSessionRepo, context: &Context) -> maho_agent::harness::session::jsonl::JsonlSessionMetadata {
    repo.list(Some(JsonlSessionListOptions { cwd: None }), context)
        .await
        .expect("list")
        .into_iter()
        .next()
        .expect("metadata")
}

#[tokio::test]
async fn persists_metadata_and_filters_discovery_by_cwd() {
    let root = temp_root("metadata");
    let context = BACKGROUND_CONTEXT.clone();
    let file_system = node_env(&root);
    let repo = repo(file_system.clone());
    let mut options = create_options("child", "/workspace");
    options.parent_session_id = Some("parent".to_owned());
    let session = repo.create(options, &context).await.expect("create");
    let metadata = session.metadata().clone();

    assert_eq!(metadata.id, "child");
    assert_eq!(metadata.created_at, NOW);
    assert_eq!(metadata.storage_version, JSONL_STORAGE_VERSION);
    assert_eq!(metadata.cwd.as_deref(), Some("/workspace"));
    assert_eq!(metadata.parent_session_id.as_deref(), Some("parent"));
    session.close(&context).await;

    let workspace = repo
        .list(
            Some(JsonlSessionListOptions {
                cwd: Some("/workspace".to_owned()),
            }),
            &context,
        )
        .await
        .expect("list workspace");
    assert_eq!(workspace.len(), 1);
    assert_eq!(workspace[0].id, "child");
    assert!(workspace[0].path.contains("/sessions/--workspace--/"), "{}", workspace[0].path);
    assert!(workspace[0].path.ends_with("_child.jsonl"), "{}", workspace[0].path);
    assert!(workspace[0].modified_at > 0);

    let other = repo
        .list(
            Some(JsonlSessionListOptions {
                cwd: Some("/other".to_owned()),
            }),
            &context,
        )
        .await
        .expect("list other");
    assert!(other.is_empty());

    let first_line = file_system
        .read_text_lines(&workspace[0].path, Some(1), &context)
        .await
        .expect("read header")
        .into_iter()
        .next()
        .expect("header line");
    let header: serde_json::Value = serde_json::from_str(&first_line).expect("header json");
    assert_eq!(header["v"], 4);
    assert_eq!(header["kind"], "header");
    assert_eq!(header["id"], "child");
    assert_eq!(header["storageVersion"], JSONL_STORAGE_VERSION);
    assert_eq!(header["createdAt"], NOW);
    assert_eq!(header["cwd"], "/workspace");
    assert_eq!(header["parentSessionId"], "parent");
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn atomically_publishes_a_branchless_session_header() {
    let root = temp_root("atomic-header");
    let context = BACKGROUND_CONTEXT.clone();
    let observing = Arc::new(support::ObservingFileSystem::new(node_env(&root), 0));
    let repo = repo(observing.clone());
    let session = repo
        .create(create_options("session", "/workspace"), &context)
        .await
        .expect("create");
    let metadata = only_metadata(&repo, &context).await;

    let publications = observing.publications();
    assert_eq!(publications.len(), 1, "expected exactly one atomic publication");
    let publication = &publications[0];
    assert_eq!(publication.destination_path, metadata.path);
    assert!(!publication.destination_existed);
    let lines: Vec<&str> = publication.staged_content.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 1);
    let header: serde_json::Value = serde_json::from_str(lines[0]).expect("header json");
    assert_eq!(header["kind"], "header");
    assert_eq!(header["id"], "session");
    assert_eq!(
        std::fs::read_to_string(&publication.destination_path).expect("destination"),
        publication.staged_content
    );
    assert!(!std::path::Path::new(&publication.source_path).exists());

    session.close(&context).await;
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn keeps_an_explicit_session_mutation_through_commit_until_end() {
    let root = temp_root("explicit-mutation");
    let context = BACKGROUND_CONTEXT.clone();
    let repo = repo(node_env(&root));
    let session = repo
        .create(create_options("session", "/workspace"), &context)
        .await
        .expect("create");
    let mutation = session.begin_mutation(&context).await.expect("mutation");
    let queued_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let queued = session.mutate(
        Arc::new({
            let queued_started = queued_started.clone();
            move |_mutator, _context| {
                queued_started.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move { Ok(serde_json::Value::Null) })
            }
        }),
        &context,
    );

    let result = mutation
        .commit(vec![Write::Value(set_value(&session_name(), serde_json::json!("explicit")))], &context)
        .await
        .expect("commit");
    assert_eq!(result.seqs.len(), 1);
    assert!(!queued_started.load(std::sync::atomic::Ordering::SeqCst));
    let stored = mutation
        .get_value(&session_name(), &context)
        .await
        .expect("value")
        .expect("value present");
    assert_eq!(stored.value, serde_json::json!("explicit"));
    mutation.end(&context).await;
    queued.await.expect("queued");
    assert!(queued_started.load(std::sync::atomic::Ordering::SeqCst));

    let metadata = only_metadata(&repo, &context).await;
    session.close(&context).await;
    let reopened = repo.open(metadata, &context).await.expect("reopen");
    assert_eq!(reopened.get_name(&context).await.expect("name"), Some("explicit".to_owned()));
    reopened.close(&context).await;
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn rejects_unsupported_storage_versions_without_repairing_a_torn_tail() {
    let root = temp_root("unsupported-version");
    let context = BACKGROUND_CONTEXT.clone();
    let file_system = node_env(&root);
    let repo = repo(file_system.clone());
    let session = repo
        .create(create_options("future", "/workspace"), &context)
        .await
        .expect("create");
    let metadata = only_metadata(&repo, &context).await;
    session.close(&context).await;

    let content = file_system
        .read_text_file(&metadata.path, &context)
        .await
        .expect("read");
    let mut lines: Vec<String> = content.trim_end().split('\n').map(str::to_owned).collect();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).expect("header");
    let unsupported = JSONL_STORAGE_VERSION + 1;
    header["storageVersion"] = serde_json::json!(unsupported);
    lines[0] = header.to_string();
    let corrupted = format!("{}\n{{\"kind\":\"entry\"", lines.join("\n"));
    file_system
        .write_file(&metadata.path, corrupted.as_bytes(), &context)
        .await
        .expect("write");

    let error = repo.open(metadata.clone(), &context).await.err().expect("unsupported version");
    assert!(
        error
            .message
            .contains(&format!("unsupported storage version {unsupported}")),
        "{}",
        error.message
    );
    assert_eq!(
        file_system
            .read_text_file(&metadata.path, &context)
            .await
            .expect("reread"),
        corrupted
    );
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn keeps_fork_destinations_claimed_until_close_and_rejects_deleting_open_sessions() {
    let root = temp_root("fork-claims");
    let context = BACKGROUND_CONTEXT.clone();
    let repo = repo(node_env(&root));
    let source = repo
        .create(create_options("source", "/workspace"), &context)
        .await
        .expect("create");
    let source_metadata = only_metadata(&repo, &context).await;
    let fork = repo
        .fork(
            source_metadata,
            ForkOptions::Tree {
                id: Some("fork".to_owned()),
            },
            &context,
        )
        .await
        .expect("fork");
    let fork_metadata = repo
        .list(Some(JsonlSessionListOptions { cwd: None }), &context)
        .await
        .expect("list")
        .into_iter()
        .find(|metadata| metadata.id == "fork")
        .expect("fork metadata");

    let open_error = repo
        .open(fork_metadata.clone(), &context)
        .await
        .err()
        .expect("already open");
    assert!(open_error.message.contains("already open"), "{}", open_error.message);
    let delete_error = repo
        .delete(fork_metadata.clone(), &context)
        .await
        .expect_err("open session cannot be deleted");
    assert!(delete_error.message.contains("open"), "{}", delete_error.message);
    fork.close(&context).await;

    let reopened = repo.open(fork_metadata.clone(), &context).await.expect("reopen");
    reopened.close(&context).await;
    repo.delete(fork_metadata.clone(), &context).await.expect("delete");
    let missing = repo.open(fork_metadata, &context).await.err().expect("deleted");
    assert!(missing.message.contains("does not exist"), "{}", missing.message);

    source.close(&context).await;
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn rejects_concurrent_creates_for_the_same_working_directory_id() {
    let root = temp_root("concurrent-create");
    let context = BACKGROUND_CONTEXT.clone();
    let repo = repo(node_env(&root));

    let (first, second) = tokio::join!(
        repo.create(create_options("session", "/workspace"), &context),
        repo.create(create_options("session", "/workspace"), &context)
    );
    let fulfilled = [first.is_ok(), second.is_ok()].iter().filter(|ok| **ok).count();
    assert_eq!(fulfilled, 1);
    assert_eq!(
        repo.list(Some(JsonlSessionListOptions { cwd: None }), &context)
            .await
            .expect("list")
            .len(),
        1
    );
    if let Ok(session) = first {
        session.close(&context).await;
    }
    if let Ok(session) = second {
        session.close(&context).await;
    }
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn allows_the_same_id_to_be_active_in_different_working_directories() {
    let root = temp_root("same-id");
    let context = BACKGROUND_CONTEXT.clone();
    let repo = repo(node_env(&root));
    let first = repo
        .create(create_options("shared", "/workspace-a"), &context)
        .await
        .expect("first");
    let second = repo
        .create(create_options("shared", "/workspace-b"), &context)
        .await
        .expect("second");

    let listing = repo.list(None, &context).await.expect("list");
    assert_eq!(
        listing
            .iter()
            .map(|metadata| (metadata.cwd.clone(), metadata.id.clone()))
            .collect::<Vec<(String, String)>>(),
        vec![
            ("/workspace-a".to_owned(), "shared".to_owned()),
            ("/workspace-b".to_owned(), "shared".to_owned()),
        ]
    );
    assert_ne!(listing[0].path, listing[1].path);
    let duplicate = repo
        .create(create_options("shared", "/workspace-a"), &context)
        .await
        .err()
        .expect("duplicate");
    assert!(duplicate.message.contains("already exists"), "{}", duplicate.message);

    let (first_metadata, second_metadata) = (listing[0].clone(), listing[1].clone());
    first.close(&context).await;
    second.close(&context).await;
    let (reopened_first, reopened_second) = tokio::join!(
        repo.open(first_metadata, &context),
        repo.open(second_metadata, &context)
    );
    reopened_first.expect("reopen first").close(&context).await;
    reopened_second.expect("reopen second").close(&context).await;
    repo.close(&context).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}
