use maho_codemode::{extension::eval_notifier::*, tool::detached_cell_notification::EvalDetachedCellNotification};
use maho_ext_api::{DeliverAs, ExtensionMode};

fn cells() -> Vec<EvalDetachedCellNotification> { vec![EvalDetachedCellNotification { cell_id:"cell".into(),content:"notice".into() }] }

#[test]
fn delivery_is_internal_and_steers_with_provenance() {
    let (message,options)=EvalNotifier::default().notification_message(&cells(),EvalNotifyMode::Wake,Some((ExtensionMode::Tui,true))).unwrap();
    assert_eq!(message.custom_type,EVAL_NOTIFICATION_CUSTOM_TYPE);
    assert!(!message.display);
    assert!(options.trigger_turn);
    assert_eq!(options.deliver_as,Some(DeliverAs::Steer));
}

#[test]
fn reset_scopes_deduplication_to_session_generation() {
    let mut notifier=EvalNotifier::default();
    let context=Some((ExtensionMode::Tui,true));
    assert!(notifier.notification_message(&cells(),EvalNotifyMode::Wake,context).is_some());
    assert!(notifier.notification_message(&cells(),EvalNotifyMode::Wake,context).is_none());
    notifier.reset();
    assert!(notifier.notification_message(&cells(),EvalNotifyMode::Wake,context).is_some());
}

#[test]
fn suppressed_context_does_not_consume_notification_and_next_turn_follows_up() {
    let mut notifier=EvalNotifier::default();
    for context in [None,Some((ExtensionMode::Print,true)),Some((ExtensionMode::Json,true)),Some((ExtensionMode::Tui,false))] {
        assert!(notifier.notification_message(&cells(),EvalNotifyMode::Wake,context).is_none());
    }
    assert!(notifier.notification_message(&cells(),EvalNotifyMode::Off,Some((ExtensionMode::Tui,true))).is_none());
    let (_,options)=notifier.notification_message(&cells(),EvalNotifyMode::NextTurn,Some((ExtensionMode::Rpc,true))).unwrap();
    assert_eq!(options.deliver_as,Some(DeliverAs::FollowUp));
}
