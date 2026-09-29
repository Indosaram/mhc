use super::*;
use pretty_assertions::assert_eq;

fn recording_state() -> (WorkspaceDocumentState, Arc<Mutex<Vec<Value>>>) {
    let notifications: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = notifications.clone();
    let send: SendNotificationFn = Arc::new(move |method, params| {
        if method == "workspace/didChangeWatchedFiles" {
            sink.lock().expect("lock").push(params);
        }
        Box::pin(async { Ok(()) })
    });
    let state = WorkspaceDocumentState::new(
        send,
        Arc::new(|_| {}),
        WorkspaceDocumentStateOptions {
            now: None,
            versionless_publish_quiescence_ms: Some(0.0),
        },
    );
    (state, notifications)
}

fn changed_paths() -> Vec<String> {
    (0..129)
        .map(|index| format!("/workspace/file-{index}.ts"))
        .collect()
}

fn change_types(notification: &Value) -> Vec<Value> {
    notification["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .map(|change| change["type"].clone())
        .collect()
}

#[tokio::test]
async fn given_more_closed_file_mutations_than_one_batch_when_synchronized_then_every_notification_stays_bounded()
 {
    let (documents, notifications) = recording_state();
    let changed_paths = changed_paths();
    let operations = changed_paths
        .iter()
        .map(|path| WorkspaceMutation::Create {
            path: path.clone(),
            replaced: false,
        })
        .collect();

    documents
        .synchronize(&WorkspaceMutationDelta {
            operations,
            changed_paths,
        })
        .await
        .expect("synchronize");

    let notifications = notifications.lock().expect("lock");
    assert_eq!(notifications.len(), 2);
    assert_eq!(change_types(&notifications[0]).len(), 128);
    assert_eq!(change_types(&notifications[1]).len(), 1);
}

#[tokio::test]
async fn given_changed_closed_files_when_synchronized_then_sends_bounded_changed_watched_file_notifications()
 {
    let (documents, notifications) = recording_state();
    let changed_paths = changed_paths();
    let operations = changed_paths
        .iter()
        .map(|path| WorkspaceMutation::Text {
            path: path.clone(),
            before_text: "before".to_string(),
            after_text: "after".to_string(),
        })
        .collect();

    documents
        .synchronize(&WorkspaceMutationDelta {
            operations,
            changed_paths,
        })
        .await
        .expect("synchronize");

    let notifications = notifications.lock().expect("lock");
    assert_eq!(notifications.len(), 2);
    assert_eq!(change_types(&notifications[0]), vec![json!(2); 128]);
    assert_eq!(change_types(&notifications[1]), vec![json!(2)]);
}
