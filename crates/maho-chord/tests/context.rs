use maho_chord::{context::*, types::*};
use std::{sync::Arc, time::Duration};

#[test]
fn distinct_roots_and_identity_keyed_immutable_layers() {
    assert!(!Arc::ptr_eq(&BACKGROUND_CONTEXT, &TODO_CONTEXT));
    let first = create_context_key::<String>("value");
    let duplicate = create_context_key::<String>("value");
    let parent = with_context_value(&first, "one".into(), &BACKGROUND_CONTEXT);
    let child = with_context_value(&first, "two".into(), &parent);
    assert_eq!(context_value(&parent, &first).as_deref(), Some("one"));
    assert_eq!(context_value(&child, &first).as_deref(), Some("two"));
    assert!(context_value(&child, &duplicate).is_none());
    assert!(context_value(&BACKGROUND_CONTEXT, &first).is_none());
}

#[tokio::test]
async fn cancellation_inherits_and_masks_without_cancelling_the_work() -> Result<(), ServiceError> {
    let (parent, cancel_parent) = with_cancel(&BACKGROUND_CONTEXT);
    let (child, cancel_child) = with_cancel(&parent);
    let (sibling, _) = with_cancel(&parent);
    let cleanup = without_abort_signal(&child);
    let reason: Arc<dyn std::error::Error + Send + Sync> = Arc::new(std::io::Error::other("child"));
    cancel_child.cancel_with_reason(reason.clone());
    let signal = child.abort_signal().ok_or("missing signal")?;
    assert!(signal.aborted());
    assert!(Arc::ptr_eq(&signal.reason().ok_or("missing reason")?, &reason));
    assert!(!parent.abort_signal().ok_or("missing parent signal")?.aborted());
    assert!(!sibling.abort_signal().ok_or("missing sibling signal")?.aborted());
    assert!(cleanup.abort_signal().is_none());
    cancel_parent.cancel();
    assert!(Arc::ptr_eq(&signal.reason().ok_or("missing retained reason")?, &reason));
    let sibling_signal = sibling.abort_signal().ok_or("missing sibling signal")?;
    tokio::time::timeout(Duration::from_secs(1), sibling_signal.cancelled()).await?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut work = tokio::spawn(receiver);
    assert!(await_with_context(&mut work, &child).await.is_err());
    sender.send("completed later").map_err(|_| "receiver dropped")?;
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), work).await??? , "completed later");
    Ok(())
}

#[test]
fn already_aborted_combination_selects_parent_before_child() -> Result<(), ServiceError> {
    // Given both inputs are aborted, even though the child aborted first.
    let (parent, cancel_parent) = with_cancel(&BACKGROUND_CONTEXT);
    let (child, cancel_child) = with_cancel(&BACKGROUND_CONTEXT);
    let parent_reason: CancellationReason = Arc::new(std::io::Error::other("parent"));
    cancel_child.cancel_with_reason(Arc::new(std::io::Error::other("child")));
    cancel_parent.cancel_with_reason(parent_reason.clone());
    // When combining already-aborted inputs, input list order wins.
    let combined = with_abort_signal(child.abort_signal().ok_or("missing child")?, &parent);
    // Then the selected reason retains identity and is immutable.
    let reason = combined.abort_signal().ok_or("missing combined")?.reason().ok_or("missing reason")?;
    assert!(Arc::ptr_eq(&reason, &parent_reason));
    cancel_parent.cancel_with_reason(Arc::new(std::io::Error::other("ignored")));
    assert!(Arc::ptr_eq(&reason, &parent.abort_signal().ok_or("missing parent")?.reason().ok_or("missing reason")?));
    Ok(())
}

#[test]
fn nested_combination_retains_first_abort_after_every_input_aborts() -> Result<(), ServiceError> {
    // Given three initially live independently cancellable signals.
    let (first, cancel_first) = with_cancel(&BACKGROUND_CONTEXT);
    let (second, cancel_second) = with_cancel(&BACKGROUND_CONTEXT);
    let (third, cancel_third) = with_cancel(&BACKGROUND_CONTEXT);
    let pair = with_abort_signal(second.abort_signal().ok_or("missing second")?, &first);
    let combined = with_abort_signal(third.abort_signal().ok_or("missing third")?, &pair);
    let selected: CancellationReason = Arc::new(std::io::Error::other("second wins"));
    // When all signals abort without polling the combined signal between them.
    cancel_second.cancel_with_reason(selected.clone()); cancel_third.cancel(); cancel_first.cancel();
    // Then the earliest event wins, not parent traversal or waiter polling order.
    assert!(Arc::ptr_eq(&combined.abort_signal().ok_or("missing combined")?.reason().ok_or("missing reason")?, &selected));
    Ok(())
}
