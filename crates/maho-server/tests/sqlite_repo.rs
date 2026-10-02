use maho_agent::harness::{context::background_context, session::*};
use maho_server::sqlite::SqliteSessionRepo;
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn active_source_fork_uses_storage_connection_after_path_is_renamed() {
    let directory=tempfile::tempdir().unwrap();
    let repo=SqliteSessionRepo::new(directory.path().into(),None,Arc::new(||123));
    let context=background_context();
    let source=repo.create(SessionCreateOptions {id:Some("source".into()),..Default::default()},&context).await.unwrap();
    source.set_name(Some("live".into()),&context).await.unwrap();
    let path=directory.path().join("source.sqlite");
    let moved=directory.path().join("preserved.sqlite");
    std::fs::rename(&path,&moved).unwrap();
    let result=repo.fork(source.metadata().clone(),ForkOptions::Tree {id:Some("copy".into())},&context).await;
    std::fs::rename(&moved,&path).unwrap();
    let fork=result.unwrap();
    assert_eq!(fork.get_name(&context).await.unwrap(),Some("live".into()));
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
    let repo = SqliteSessionRepo::new(directory.path().into(),Some(path.clone()),Arc::new(||123));
    let context = background_context();
    let source = repo.create(SessionCreateOptions {id:Some("source".into()),..Default::default()},&context).await.unwrap();
    let metadata = source.metadata().clone();
    source.close(&context).await;
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_fork BEFORE INSERT ON scalar_values WHEN NEW.session_id='copy' BEGIN SELECT RAISE(ABORT,'faux copy failure'); END;").unwrap();
    assert!(repo.fork(metadata.clone(),ForkOptions::Tree {id:Some("copy".into())},&context).await.is_err());
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sessions WHERE id='copy'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    db.execute_batch("DROP TRIGGER reject_fork").unwrap();
    let fork = repo.fork(metadata,ForkOptions::Tree {id:Some("copy".into())},&context).await.unwrap();
    fork.close(&context).await;
    repo.close(&context).await;
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
