use maho_ext_pi_goal::goal::{lifecycle::GoalLifecycle,store::*,types::*};
#[test]
fn extension_wires_tools_command_and_hooks_together(){
    use maho_ext_api::*;
    use std::sync::Arc;
    let temp=tempfile::tempdir().expect("fixture");let path=temp.path().to_owned();
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-goal",path.clone(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_pi_goal::index::register_goal_extension(&mut api,Arc::new(move|ctx|GoalStoreRef{base_dir:path.clone(),thread_id:ctx.session_manager.session_id().into()}),Arc::new(|_|Ok(()))).expect("extension registration");
    assert_eq!(api.registered.tools.iter().map(|tool|tool.definition.name.as_str()).collect::<Vec<_>>(),["create_goal","update_goal","get_goal"]);
    assert_eq!(api.registered.handlers.len(),6);
    assert_eq!(api.registered.commands.len(),1);
}
#[test]
fn native_lifecycle_registers_all_six_event_handlers(){
    use maho_ext_api::*;
    use std::sync::Arc;
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-goal","/fixture".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    let resolve=Arc::new(|_:&ExtensionContext|GoalStoreRef{base_dir:"/fixture".into(),thread_id:"fixture".into()});
    maho_ext_pi_goal::goal::lifecycle::register_goal_lifecycle(&mut api,resolve,Arc::new(|_|Ok(())));
    for kind in [EventKind::SessionStart,EventKind::BeforeAgentStart,EventKind::AgentStart,EventKind::MessageEnd,EventKind::AgentEnd,EventKind::SessionShutdown]{assert_eq!(api.registered.handlers[&kind].len(),1);}
    assert_eq!(api.registered.handlers.len(),6);
}
#[test]fn streamed_usage_flushes_once(){let temp=tempfile::tempdir().unwrap();let reference=GoalStoreRef{base_dir:temp.path().into(),thread_id:"thread".into()};let goal=create_goal_at(&reference,"objective",10,"id".into()).unwrap();let mut lifecycle=GoalLifecycle::default();lifecycle.begin_agent_goal_accounting(&goal,10000);let message=serde_json::json!({"role":"assistant","usage":{"input":10,"output":5}});lifecycle.turn_usage.note_message_end(&message);let first=lifecycle.account_current_agent_turn(&reference,GoalAccountingMode::Active,None,11500).unwrap().unwrap();assert_eq!((first.tokens_used,first.time_used_seconds),(15,2));let last=lifecycle.account_current_agent_turn(&reference,GoalAccountingMode::Active,Some(&[message]),12500).unwrap().unwrap();assert_eq!((last.tokens_used,last.time_used_seconds),(15,3));}
#[test]fn replaced_goal_does_not_receive_stale_usage(){let temp=tempfile::tempdir().unwrap();let reference=GoalStoreRef{base_dir:temp.path().into(),thread_id:"thread".into()};let goal=create_goal_at(&reference,"objective",10,"id".into()).unwrap();let mut lifecycle=GoalLifecycle::default();lifecycle.begin_agent_goal_accounting(&goal,10000);update_goal_at(&reference,&GoalUpdate{objective:Some("replacement".into()),..GoalUpdate::default()},GoalUpdateSource::User,10,"replacement-id".into()).unwrap();lifecycle.turn_usage.note_message_end(&serde_json::json!({"role":"assistant","usage":{"input":10,"output":5}}));let result=lifecycle.account_current_agent_turn(&reference,GoalAccountingMode::Active,None,15000).unwrap().unwrap();assert_eq!(result.id,"replacement-id");assert_eq!(result.tokens_used,0);assert!(lifecycle.agent_goal_accounting.is_none());}
