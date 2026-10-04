mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_ultrawork::SessionArming;
use maho_omo_todo_fanout_reminder::TodoFanoutReminderComponent;
struct AnonymousSession;
impl ToolSessionManager for AnonymousSession {fn session_id(&self)->&str{""}fn session_file(&self)->Option<&std::path::Path>{None}}
impl SessionManager for AnonymousSession {fn get_entries(&self)->Vec<SessionEntry>{vec![]}fn get_branch(&self)->Vec<SessionEntry>{vec![]}fn get_leaf_id(&self)->Option<String>{None}fn get_session_name(&self)->Option<String>{None}}
fn registered(armed:bool)->ExtensionApi {
    let mut arming=SessionArming::default();if armed { arming.mark_armed(Some("session")); }
    let mut api=ExtensionApi::new(LoadedExtension::new("reminder","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    TodoFanoutReminderComponent{arming:Arc::new(Mutex::new(arming))}.register(&mut api);api
}
fn result(op:&str,error:bool)->ExtensionEvent { ExtensionEvent::ToolResult(ToolResultEvent{tool_name:"todo".into(),tool_call_id:"call".into(),input:serde_json_value(op),content:vec![ToolContent::text("original")],details:None,is_error:error,usage:None}) }
fn serde_json_value(op:&str)->JsonValue { JsonValue::Object([("op".into(),JsonValue::String(op.into()))].into_iter().collect()) }
async fn dispatch(api:&ExtensionApi,event:&mut ExtensionEvent)->EventResult { api.registered.handlers[&event.kind()][0](event,&support::context()).await.expect("test dispatch") }
#[tokio::test] async fn armed_init_appends_reminder() { let api=registered(true);let EventResult::ToolResult(result)=dispatch(&api,&mut result("init",false)).await else { panic!("expected transform"); };assert_eq!(result.content.expect("content").len(),2); }
#[tokio::test] async fn repeat_not_duplicated() { let api=registered(true);dispatch(&api,&mut result("init",false)).await;assert!(matches!(dispatch(&api,&mut result("append",false)).await,EventResult::None)); }
#[tokio::test] async fn unarmed_no_reminder() { let api=registered(false);assert!(matches!(dispatch(&api,&mut result("init",false)).await,EventResult::None)); }
#[tokio::test] async fn errors_no_reminder() { let api=registered(true);assert!(matches!(dispatch(&api,&mut result("init",true)).await,EventResult::None)); }
#[tokio::test] async fn other_tools_no_reminder() { let api=registered(true);let mut event=result("init",false);if let ExtensionEvent::ToolResult(e)=&mut event { e.tool_name="read".into(); }assert!(matches!(dispatch(&api,&mut event).await,EventResult::None)); }
#[tokio::test] async fn disabled_no_reminder() { let api=registered(true);api.set_flag("omo-senpi-todo-fanout-reminder-disabled",FlagValue::Boolean(true));assert!(matches!(dispatch(&api,&mut result("init",false)).await,EventResult::None)); }
#[tokio::test] async fn append_triggers() { let api=registered(true);assert!(matches!(dispatch(&api,&mut result("append",false)).await,EventResult::ToolResult(_))); }
#[tokio::test] async fn done_not_task_adding() { let api=registered(true);assert!(matches!(dispatch(&api,&mut result("done",false)).await,EventResult::None)); }
#[tokio::test] async fn rejected_compact_preserves_reminder_state() { let api=registered(true);dispatch(&api,&mut result("init",false)).await;dispatch(&api,&mut ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{reason:CompactionReason::Manual,request_id:"id".into(),rejection_cause:CompactionRejectionCause::CancelledByExtension})).await;assert!(matches!(dispatch(&api,&mut result("init",false)).await,EventResult::None)); }
#[tokio::test] async fn shutdown_clears_reminder() { let api=registered(true);dispatch(&api,&mut result("init",false)).await;dispatch(&api,&mut ExtensionEvent::SessionShutdown(SessionShutdownEvent{reason:SessionReason::Quit,target_session_file:None,signal:None})).await;assert!(matches!(dispatch(&api,&mut result("init",false)).await,EventResult::ToolResult(_))); }
#[tokio::test] async fn view_then_append_triggers() { let api=registered(true);assert!(matches!(dispatch(&api,&mut result("view",false)).await,EventResult::None));assert!(matches!(dispatch(&api,&mut result("append",false)).await,EventResult::ToolResult(_))); }
#[tokio::test] async fn accepted_compact_clears_reminder_gate() {let api=registered(true);dispatch(&api,&mut result("init",false)).await;let mut event=ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted{reason:CompactionReason::Manual,request_id:"id".into(),compaction_entry:SessionEntry{id:"entry".into(),parent_id:None,timestamp:String::new(),kind:"compaction".into(),data:JsonValue::Null},from_extension:false,will_retry:false});dispatch(&api,&mut event).await;assert!(matches!(dispatch(&api,&mut result("init",false)).await,EventResult::ToolResult(_)));}
#[tokio::test] async fn anonymous_slot_reminds_only_once() {let mut arming=SessionArming::default();arming.mark_armed(None);let mut api=ExtensionApi::new(LoadedExtension::new("reminder","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TodoFanoutReminderComponent{arming:Arc::new(Mutex::new(arming))}.register(&mut api);let mut ctx=support::context();ctx.session_manager=Arc::new(AnonymousSession);for expected in [true,false] {assert_eq!(matches!(api.registered.handlers[&EventKind::ToolResult][0](&mut result("init",false),&ctx).await.expect("dispatch"),EventResult::ToolResult(_)),expected);}}
