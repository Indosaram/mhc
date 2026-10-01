mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_fallback_architect::index::FallbackArchitectComponent;
#[derive(Default)] struct Actions(Mutex<Vec<CustomMessage>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure>{self.0.lock().expect("messages").push(m);Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected user message")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
fn register(gate:bool)->(ExtensionApi,Arc<Actions>) {let a=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(a.clone());let mut api=ExtensionApi::new(LoadedExtension::new("fallback","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);FallbackArchitectComponent{has_architect_category:Some(Arc::new(move |_,_|gate))}.register(&mut api);(api,a)}
fn model(id:&str)->Model {serde_json::from_value(serde_json::json!({"id":id,"name":id,"api":"anthropic-messages","provider":"anthropic","baseUrl":"http://localhost","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100})).expect("model")}
fn selection(source:ModelSelectSource)->ExtensionEvent {ExtensionEvent::ModelSelect(ModelSelectEvent{model:model("fallback"),previous_model:Some(model("claude-fable-5")),source,system_prompt:String::new(),system_prompt_options:BuildSystemPromptOptions::default()})}
fn refusal()->ExtensionEvent {ExtensionEvent::MessageEnd{message:serde_json::from_value(serde_json::json!({"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-fable-5","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"error","errorMessage":"This request triggered restrictions on output content and was blocked under Anthropic's Usage Policy","timestamp":0})).expect("assistant")}}
async fn dispatch(api:&ExtensionApi,mut event:ExtensionEvent)->EventResult {api.registered.handlers[&event.kind()][0](&mut event,&support::context()).await.expect("dispatch")}
#[tokio::test] async fn refusal_then_fallback_emits_hidden_and_visible() {let(api,a)=register(true);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;let m=a.0.lock().expect("messages");assert_eq!(m.len(),2);assert!(!m[0].display);assert!(m[1].display);assert!(m[1].details.is_some());}
#[tokio::test] async fn no_refusal_no_injection() {let(api,a)=register(true);dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn inactive_category_no_injection() {let(api,a)=register(false);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn manual_switch_no_injection() {let(api,a)=register(true);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Set)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn disabled_no_injection() {let(api,a)=register(true);api.set_flag("omo-senpi-fallback-architect-disabled",FlagValue::Boolean(true));dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
