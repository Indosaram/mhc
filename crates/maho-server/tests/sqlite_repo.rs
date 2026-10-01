use maho_agent::harness::{context::background_context, session::*};
use maho_server::sqlite::repo::SqliteSessionRepo;
use serde_json::json;
use std::sync::Arc;
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
