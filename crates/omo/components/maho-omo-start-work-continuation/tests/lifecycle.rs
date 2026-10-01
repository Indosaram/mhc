mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
#[derive(Default)] struct Actions(Mutex<Vec<CustomMessage>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{assert!(o.trigger_turn);assert_eq!(o.deliver_as,Some(DeliverAs::FollowUp));self.0.lock().expect("messages").push(m);Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected user message")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
#[tokio::test] async fn signature_retry_budget_and_user_reset()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let plan=root.path().join("plan.md");std::fs::write(&plan,"## TODOs\n- [ ] 1. task\n")?;let state=boulder_state::create_boulder_state(&plan.to_string_lossy(),"senpi:session",&boulder_state::WorkOwner::default());boulder_state::write_boulder_state(root.path(),&state)?;
 let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());let mut api=ExtensionApi::new(LoadedExtension::new("continuation",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);maho_omo_start_work_continuation::index::StartWorkContinuationComponent::default().register(&mut api);let mut ctx=support::context();ctx.cwd=root.path().into();
 for _ in 0..3 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;}
 assert_eq!(actions.0.lock().expect("messages").len(),2);
 let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"steer".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&ctx).await?,EventResult::Input(InputEventResult::Transform{..})));
 let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;assert_eq!(actions.0.lock().expect("messages").len(),3);Ok(())
}
