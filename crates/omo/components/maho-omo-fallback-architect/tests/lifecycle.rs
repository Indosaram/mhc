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
fn user_input(source:InputSource)->ExtensionEvent {ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"next question".into(),images:None,source,streaming_behavior:Some(StreamingBehavior::Steer)})}
#[tokio::test] async fn active_fallback_reminds_user_but_not_extension() {let(api,a)=register(true);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(matches!(dispatch(&api,user_input(InputSource::Interactive)).await,EventResult::Input(InputEventResult::Continue)));assert_eq!(a.0.lock().expect("messages").len(),3);dispatch(&api,user_input(InputSource::Extension)).await;assert_eq!(a.0.lock().expect("messages").len(),3);}
#[tokio::test] async fn revert_clears_active_reminder() {let(api,a)=register(true);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;dispatch(&api,selection(ModelSelectSource::FallbackRevert)).await;dispatch(&api,user_input(InputSource::Interactive)).await;assert_eq!(a.0.lock().expect("messages").len(),2);}
#[tokio::test] async fn successful_answer_clears_pending_refusal() {let(api,a)=register(true);dispatch(&api,refusal()).await;let mut event=refusal();if let ExtensionEvent::MessageEnd{message}=&mut event {let mut value=serde_json::to_value(&*message).expect("message");value["stopReason"]=serde_json::json!("stop");value.as_object_mut().expect("object").remove("errorMessage");*message=serde_json::from_value(value).expect("message");}dispatch(&api,event).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn refusal_then_fallback_emits_hidden_and_visible() {let(api,a)=register(true);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;let m=a.0.lock().expect("messages");assert_eq!(m.len(),2);assert!(!m[0].display);assert!(m[1].display);assert!(m[1].details.is_some());}
#[tokio::test] async fn no_refusal_no_injection() {let(api,a)=register(true);dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn inactive_category_no_injection() {let(api,a)=register(false);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn manual_switch_no_injection() {let(api,a)=register(true);dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Set)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test] async fn disabled_no_injection() {let(api,a)=register(true);api.set_flag("omo-senpi-fallback-architect-disabled",FlagValue::Boolean(true));dispatch(&api,refusal()).await;dispatch(&api,selection(ModelSelectSource::Fallback)).await;assert!(a.0.lock().expect("messages").is_empty());}
#[tokio::test]
async fn visible_notice_has_persisted_type_and_model_details() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    let messages = actions.0.lock().expect("messages");
    let notice = &messages[1];
    assert_eq!(notice.custom_type, "omo-fallback-architect:notice");
    assert!(notice.display);
    assert_eq!(notice.details, Some(serde_json::json!({
        "from": "anthropic/claude-fable-5", "to": "anthropic/fallback"
    })));
}

#[tokio::test]
async fn active_fallback_preserves_queued_input_and_hides_each_reminder() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    for behavior in [StreamingBehavior::Steer, StreamingBehavior::FollowUp] {
        let mut event = user_input(InputSource::Interactive);
        let ExtensionEvent::Input(input) = &mut event else { panic!("input") };
        input.text = "typed\nexactly".into();
        input.streaming_behavior = Some(behavior);
        let result = api.registered.handlers[&EventKind::Input][0](
            &mut event, &support::context()
        ).await.expect("dispatch");
        assert!(matches!(result, EventResult::Input(InputEventResult::Continue)));
        let ExtensionEvent::Input(input) = event else { panic!("input") };
        assert_eq!(input.text, "typed\nexactly");
        assert_eq!(input.streaming_behavior, Some(behavior));
    }
    let messages = actions.0.lock().expect("messages");
    assert_eq!(messages.len(), 4);
    for message in &messages[2..] {
        assert_eq!(message.custom_type, "omo-fallback-architect:reminder");
        assert!(!message.display);
    }
}

#[tokio::test]
async fn fallback_from_a_weaker_model_does_not_arm() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    let mut event = selection(ModelSelectSource::Fallback);
    let ExtensionEvent::ModelSelect(selected) = &mut event else { panic!("selection") };
    selected.previous_model = Some(model("claude-opus-5"));
    dispatch(&api, event).await;
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn chained_fallback_keeps_reminder_without_second_notice() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    dispatch(&api, refusal()).await;
    let mut event = selection(ModelSelectSource::Fallback);
    let ExtensionEvent::ModelSelect(selected) = &mut event else { panic!("selection") };
    selected.previous_model = Some(model("fallback"));
    selected.model = model("another-fallback");
    dispatch(&api, event).await;
    dispatch(&api, user_input(InputSource::Interactive)).await;
    let messages = actions.0.lock().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2].custom_type, "omo-fallback-architect:reminder");
}

#[tokio::test]
async fn manual_return_to_fable_clears_reminder() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    let mut event = selection(ModelSelectSource::Set);
    let ExtensionEvent::ModelSelect(selected) = &mut event else { panic!("selection") };
    selected.model = model("claude-fable-5");
    dispatch(&api, event).await;
    dispatch(&api, user_input(InputSource::Interactive)).await;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
}

#[tokio::test]
async fn disabling_an_active_component_suppresses_reminders() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    api.set_flag("omo-senpi-fallback-architect-disabled", FlagValue::Boolean(true));
    dispatch(&api, user_input(InputSource::Interactive)).await;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
}

#[test]
fn notice_renderer_truncates_each_line_without_wrapping_or_padding() {
    use senpi_task::tools::render::{Lines, LinesComponent, lines_component};
    let (api, _) = register(true);
    let message = CustomMessage {
        custom_type: "omo-fallback-architect:notice".into(),
        content: Vec::new(), display: true,
        details: Some(serde_json::json!({"from":"anthropic/claude-fable-5","to":"anthropic/fallback"})),
    };
    let renderer = &api.registered.message_renderers[&message.custom_type];
    let mut component = renderer(&message, &MessageRenderOptions::default(), &Theme::default())
        .expect("notice component");
    let reference = lines_component(Lines::Static(
        maho_omo_fallback_architect::notice::notice_lines(Some(("anthropic/claude-fable-5", "anthropic/fallback")))
    ));
    for width in [0, 10, 40, 80, 120, 2000] {
        assert_eq!(component.render(width), reference.render(width), "width {width}");
    }
}

#[tokio::test]
async fn provider_policy_rejection_arms_fallback() -> Result<(), serde_json::Error> {
    let (api, actions) = register(true);
    let message = serde_json::from_value(serde_json::json!({
        "role":"assistant", "api":"faux", "provider":"anthropic", "model":"claude-fable-5",
        "content":[],"timestamp":0,"stopReason":"error",
        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        "errorMessage":"This request triggered restrictions on request content and was blocked under Anthropic's Usage Policy."
    }))?;
    dispatch(&api, ExtensionEvent::MessageEnd { message }).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
    Ok(())
}

#[tokio::test]
async fn aborted_refusal_details_do_not_arm() {
    let (api, actions) = register(true);
    let mut event = refusal();
    let ExtensionEvent::MessageEnd { message } = &mut event else { panic!("message") };
    let mut value = serde_json::to_value(&message).expect("serialized message");
    value["stopReason"] = serde_json::json!("aborted");
    *message = serde_json::from_value(value).expect("aborted message");
    dispatch(&api, event).await;
    dispatch(&api, selection(ModelSelectSource::Fallback)).await;
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn manual_fallback_model_selection_does_not_arm() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    dispatch(&api, selection(ModelSelectSource::Set)).await;
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn missing_previous_model_does_not_arm() {
    let (api, actions) = register(true);
    dispatch(&api, refusal()).await;
    let mut event = selection(ModelSelectSource::Fallback);
    let ExtensionEvent::ModelSelect(selected) = &mut event else { panic!("selection") };
    selected.previous_model = None;
    dispatch(&api, event).await;
    assert!(actions.0.lock().expect("messages").is_empty());
}
