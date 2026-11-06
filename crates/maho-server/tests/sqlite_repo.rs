use maho_agent::harness::{context::background_context, session::*};
use maho_server::sqlite::SqliteSessionRepo;
use serde_json::json;
use std::sync::Arc;
#[cfg(unix)]
#[tokio::test]
async fn active_source_fork_uses_storage_connection_after_path_is_renamed() {
    let directory = tempfile::tempdir().unwrap();
    let repo = SqliteSessionRepo::new(directory.path().into(), None, Arc::new(|| 123));
    let context = background_context();
    let source = repo
        .create(
            SessionCreateOptions {
                id: Some("source".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    source
        .set_name(Some("live".into()), &context)
        .await
        .unwrap();
    let path = directory.path().join("source.sqlite");
    let moved = directory.path().join("preserved.sqlite");
    std::fs::rename(&path, &moved).unwrap();
    let result = repo
        .fork(
            source.metadata().clone(),
            ForkOptions::Tree {
                id: Some("copy".into()),
            },
            &context,
        )
        .await;
    std::fs::rename(&moved, &path).unwrap();
    let fork = result.unwrap();
    assert_eq!(fork.get_name(&context).await.unwrap(), Some("live".into()));
    fork.close(&context).await;
    source.close(&context).await;
    repo.close(&context).await;
}

#[tokio::test]
async fn repository_creates_reopens_forks_and_deletes_unicode_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let repo = SqliteSessionRepo::new(directory.path().into(), None, Arc::new(|| 123));
    let context = background_context();
    std::fs::write(
        directory.path().join("unrelated.sqlite"),
        "not a SQLite database",
    )
    .unwrap();
    let session = repo
        .create(
            SessionCreateOptions {
                id: Some("한/글".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    let metadata = session.metadata().clone();
    assert!(repo.open(metadata.clone(), &context).await.is_err());
    assert!(repo.delete(metadata.clone(), &context).await.is_err());
    session
        .set_name(Some("test".into()), &context)
        .await
        .unwrap();
    assert_eq!(
        repo.list(None, &context).await.unwrap(),
        vec![metadata.clone()]
    );
    let fork = repo
        .fork(
            metadata.clone(),
            ForkOptions::Tree {
                id: Some("copy".into()),
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(fork.get_name(&context).await.unwrap(), Some("test".into()));
    fork.close(&context).await;
    session.close(&context).await;
    let reopened = repo.open(metadata.clone(), &context).await.unwrap();
    assert_eq!(
        reopened.get_name(&context).await.unwrap(),
        Some("test".into())
    );
    reopened.close(&context).await;
    repo.delete(metadata, &context).await.unwrap();
    assert_eq!(repo.list(None, &context).await.unwrap().len(), 1);
    repo.close(&context).await;
    assert!(repo.list(None, &context).await.is_err());
}
#[tokio::test]
async fn failed_fork_initialization_rolls_back_destination_and_allows_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.sqlite");
    let repo = SqliteSessionRepo::new(
        directory.path().into(),
        Some(path.clone()),
        Arc::new(|| 123),
    );
    let context = background_context();
    let source = repo
        .create(
            SessionCreateOptions {
                id: Some("source".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    let metadata = source.metadata().clone();
    source.close(&context).await;
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_fork BEFORE INSERT ON scalar_values WHEN NEW.session_id='copy' BEGIN SELECT RAISE(ABORT,'faux copy failure'); END;").unwrap();
    assert!(
        repo.fork(
            metadata.clone(),
            ForkOptions::Tree {
                id: Some("copy".into())
            },
            &context
        )
        .await
        .is_err()
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions WHERE id='copy'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER reject_fork").unwrap();
    let fork = repo
        .fork(
            metadata,
            ForkOptions::Tree {
                id: Some("copy".into()),
            },
            &context,
        )
        .await
        .unwrap();
    fork.close(&context).await;
    repo.close(&context).await;
}

#[tokio::test]
async fn opened_sessions_use_the_admitted_read_facade_and_reject_reads_after_close() {
    let directory = tempfile::tempdir().unwrap();
    let repo = SqliteSessionRepo::new(directory.path().into(), None, Arc::new(|| 123));
    let context = background_context();
    let session = repo
        .create(
            SessionCreateOptions {
                id: Some("facade".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    assert!(
        session.get_entries(Vec::new(), &context).await.is_ok(),
        "admitted read through the facade"
    );
    repo.close(&context).await;
    assert_eq!(
        session
            .get_entries(Vec::new(), &context)
            .await
            .unwrap_err()
            .kind,
        SessionErrorKind::Closed,
        "repository close drains the facade and rejects later reads"
    );
}

#[tokio::test]
async fn shared_container_deletion_preserves_other_session() {
    let directory = tempfile::tempdir().unwrap();
    let repo = SqliteSessionRepo::new(
        directory.path().into(),
        Some(directory.path().join("shared.sqlite")),
        Arc::new(|| 123),
    );
    let context = background_context();
    let first = repo
        .create(
            SessionCreateOptions {
                id: Some("first".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    let metadata = first.metadata().clone();
    first.close(&context).await;
    let second = repo
        .create(
            SessionCreateOptions {
                id: Some("second".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    second
        .set_value(&value("test", "key").unwrap(), json!(true), &context)
        .await
        .unwrap();
    repo.delete(metadata, &context).await.unwrap();
    assert_eq!(repo.list(None, &context).await.unwrap().len(), 1);
    assert_eq!(
        second
            .get_value(&value("test", "key").unwrap(), &context)
            .await
            .unwrap()
            .unwrap()
            .value,
        json!(true)
    );
    repo.close(&context).await;
}

/// senpi repo.ts stores `path: canonicalPath` (realpath of the database file) in every metadata it hands out.
#[cfg(unix)]
#[tokio::test]
async fn metadata_carries_the_canonical_database_path() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let repo = SqliteSessionRepo::new(linked.clone(), None, Arc::new(|| 123));
    let context = background_context();
    let canonical = |name: &str| {
        std::fs::canonicalize(real.join(name))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let session = repo
        .create(
            SessionCreateOptions {
                id: Some("source".into()),
                ..Default::default()
            },
            &context,
        )
        .await
        .unwrap();
    let metadata = session.metadata().clone();
    assert_eq!(
        metadata.path.as_deref(),
        Some(canonical("source.sqlite").as_str())
    );
    let fork = repo
        .fork(
            metadata.clone(),
            ForkOptions::Tree {
                id: Some("copy".into()),
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(
        fork.metadata().path.as_deref(),
        Some(canonical("copy.sqlite").as_str())
    );
    fork.close(&context).await;
    session.close(&context).await;
    let listed = repo.list(None, &context).await.unwrap();
    let mut paths = listed
        .iter()
        .map(|row| row.path.clone().unwrap())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(
        paths,
        vec![canonical("copy.sqlite"), canonical("source.sqlite")]
    );
    let reopened = repo.open(metadata.clone(), &context).await.unwrap();
    assert_eq!(reopened.metadata(), &metadata);
    reopened.close(&context).await;
    repo.close(&context).await;
}

/// Metadata serialized before the `path` field existed still loads.
#[test]
fn metadata_without_a_path_still_deserializes() {
    let old: SessionMetadata =
        serde_json::from_value(json!({"id":"s","createdAt":1,"storageVersion":1})).unwrap();
    assert_eq!(old.path, None);
}

/// senpi repo.test.ts "uses exact physical identity for foreign fork sources and rejects foreign writable metadata".
#[tokio::test]
async fn foreign_fork_sources_use_their_own_path_and_foreign_writable_metadata_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let left_repo = SqliteSessionRepo::new(root.path().join("left"), None, Arc::new(|| 1));
    let right_repo = SqliteSessionRepo::new(root.path().join("right"), None, Arc::new(|| 1));
    let context = background_context();
    let create = SessionCreateOptions {
        id: Some("same".into()),
        ..Default::default()
    };
    let left = left_repo.create(create.clone(), &context).await.unwrap();
    let right = right_repo.create(create, &context).await.unwrap();
    left.set_name(Some("left".into()), &context).await.unwrap();
    right
        .set_name(Some("right".into()), &context)
        .await
        .unwrap();
    let fork = right_repo
        .fork(
            left.metadata().clone(),
            ForkOptions::Tree {
                id: Some("fork-left".into()),
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(fork.get_name(&context).await.unwrap(), Some("left".into()));
    right.close(&context).await;
    let opened = right_repo.open(left.metadata().clone(), &context).await;
    assert!(
        opened
            .as_ref()
            .err()
            .is_some_and(|e| e.message.contains("outside this repository")),
        "open of foreign metadata must be refused"
    );
    let deleted = right_repo.delete(left.metadata().clone(), &context).await;
    assert!(
        deleted
            .as_ref()
            .err()
            .is_some_and(|e| e.message.contains("outside this repository")),
        "delete of foreign metadata must be refused"
    );
    assert!(root.path().join("right/same.sqlite").exists());
    left.close(&context).await;
    fork.close(&context).await;
    left_repo.close(&context).await;
    right_repo.close(&context).await;
}
