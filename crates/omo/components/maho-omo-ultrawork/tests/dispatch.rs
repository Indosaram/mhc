mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_ultrawork::{UltraworkComponent,SessionArming,ULTRAWORK_REMINDER};
#[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{self.0.lock().expect("messages").push((m,o));Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
fn register()->ExtensionApi {
    let mut api=ExtensionApi::new(LoadedExtension::new("ultrawork","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    UltraworkComponent{arming:Arc::new(Mutex::new(SessionArming::default()))}.register(&mut api);api
}
async fn input(api:&ExtensionApi,text:&str,queued:bool)->EventResult {
    let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:text.into(),images:None,source:InputSource::Interactive,streaming_behavior:queued.then_some(StreamingBehavior::Steer)});
    api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch")
}
#[tokio::test] async fn queued_first_directive_byte_equality() { let api=register();let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw build",true).await else {panic!("transform");};assert_eq!(text,format!("ulw build\n{}",maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE)); }
#[tokio::test] async fn astra_model_uses_variant_directive() { let mut ctx=support::context();ctx.model=Some(Model { deprecated: None, id: "gpt-6-astra".into(), name: "gpt-6-astra".into(), api: "openai-responses".into(), provider: "openai".into(), base_url: String::new(), reasoning: false, thinking_level_map: None, prompt_cache: None, supports_assistant_prefill: None, input: vec![], cost: Default::default(), context_window: 1, max_tokens: 1, sampling_params: None, headers: None, cache_retention: None, upstream_model_id: None, service_tier: None, recover_text_tool_calls: None, compat: None, default_thinking_level: None, input_limits: None });let api=register();let mut event=ExtensionEvent::Input(InputEvent{input_id:"astra".into(),text:"ulw build".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});let EventResult::Input(InputEventResult::Transform{text,..})=api.registered.handlers[&EventKind::Input][0](&mut event,&ctx).await.expect("dispatch") else {panic!("transform");};assert_eq!(text,format!("ulw build\n{}",maho_omo_ultrawork::generated_directive::SENPI_ASTRA_ULTRAWORK_DIRECTIVE)); }
#[tokio::test] async fn context_delivery_reselects_for_current_model() { let api=register();let ctx=support::context();let mut event=ExtensionEvent::Context{messages:vec![AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage{content:maho_ai::types::UserContent::Text(maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE.into()),timestamp:0}))],entry_ids:vec![None],source:ContextEventSource::Agent};let result=api.registered.handlers[&EventKind::Context][0](&mut event,&ctx).await.expect("dispatch");let EventResult::Context{messages:Some(messages)}=result else {panic!("context transform")};let AgentMessage::Llm(maho_ai::types::Message::User(user))=&messages[0] else {panic!("user")};let maho_ai::types::UserContent::Text(text)=&user.content else {panic!("text")};assert_eq!(text,maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE); }
#[tokio::test] async fn queued_second_uses_reminder() { let api=register();input(&api,"ulw build",true).await;let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw again",true).await else {panic!("transform");};assert_eq!(text,format!("ulw again\n{ULTRAWORK_REMINDER}")); }
#[tokio::test] async fn pasted_block_arms_without_transform() { let api=register();assert!(matches!(input(&api,"<ultrawork-mode>x</ultrawork-mode> ulw",true).await,EventResult::Input(InputEventResult::Continue)));let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert!(text.ends_with(ULTRAWORK_REMINDER)); }
#[tokio::test] async fn skill_expansion_arms() { let api=register();input(&api,"/skill:ultrawork",true).await;let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert!(text.ends_with(ULTRAWORK_REMINDER)); }
#[tokio::test] async fn ordinary_input_passes() { assert!(matches!(input(&register(),"hello",true).await,EventResult::Input(InputEventResult::Continue))); }
#[tokio::test] async fn idle_injects_hidden_without_rewriting_user() {let api=register();let a=Arc::new(Actions::default());api.runtime.bind(a.clone());assert!(matches!(input(&api,"ulw build",false).await,EventResult::Input(InputEventResult::Continue)));let m=a.0.lock().expect("messages");assert_eq!(m.len(),1);assert!(!m[0].0.display);assert!(!m[0].1.trigger_turn);assert!(m[0].1.deliver_as.is_none());}
#[tokio::test] async fn accepted_compact_reinjects_full_directive() {let api=register();input(&api,"ulw",true).await;let mut event=ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted{reason:CompactionReason::Manual,request_id:"id".into(),compaction_entry:SessionEntry{id:"entry".into(),parent_id:None,timestamp:String::new(),kind:"compaction".into(),data:JsonValue::Null},from_extension:false,will_retry:false});api.registered.handlers[&EventKind::SessionCompact][0](&mut event,&support::context()).await.expect("dispatch");let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert_eq!(text,format!("ulw\n{}",maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));}
#[tokio::test] async fn rejected_compact_retains_reminder() {let api=register();input(&api,"ulw",true).await;let mut event=ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{reason:CompactionReason::Manual,request_id:"id".into(),rejection_cause:CompactionRejectionCause::CancelledByExtension});api.registered.handlers[&EventKind::SessionCompact][0](&mut event,&support::context()).await.expect("dispatch");let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert_eq!(text,format!("ulw\n{ULTRAWORK_REMINDER}"));}

#[tokio::test]
async fn follow_up_keeps_skill_command_and_directive_atomic() {
    let api = register();
    let actions = Arc::new(Actions::default());
    api.runtime.bind(actions.clone());
    let prompt = "/skill:frontend ulw polish";
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: prompt.into(),
        images: None, source: InputSource::Interactive, streaming_behavior: Some(StreamingBehavior::FollowUp) });
    let result = api.registered.handlers[&EventKind::Input][0](&mut event, &support::context()).await.expect("dispatch");
    let EventResult::Input(InputEventResult::Transform { text, .. }) = result else { panic!("transform") };
    assert_eq!(text, format!("{prompt}\n{}", maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn disabled_trigger_does_not_arm_or_inject() {
    let api = register();
    api.set_flag("omo-senpi-ultrawork-disabled", FlagValue::Boolean(true));
    assert!(matches!(input(&api, "ulw", true).await, EventResult::Input(InputEventResult::Continue)));
    api.set_flag("omo-senpi-ultrawork-disabled", FlagValue::Boolean(false));
    let EventResult::Input(InputEventResult::Transform { text, .. }) = input(&api, "ulw", true).await else { panic!("transform") };
    assert_eq!(text, format!("ulw\n{}", maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));
}

#[tokio::test]
async fn extension_trigger_never_arms_user_session() {
    let api = register();
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "ulw".into(),
        images: None, source: InputSource::Extension, streaming_behavior: None });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &support::context()).await.expect("dispatch"),
        EventResult::Input(InputEventResult::Continue)));
    let EventResult::Input(InputEventResult::Transform { text, .. }) = input(&api, "ulw", true).await else { panic!("transform") };
    assert_eq!(text, format!("ulw\n{}", maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));
}

#[tokio::test]
async fn skill_name_only_and_prefixed_variants_do_not_arm() {
    for prompt in ["/skill:myulw run", "ulw--plan"] {
        let api = register();
        assert!(matches!(input(&api, prompt, true).await, EventResult::Input(InputEventResult::Continue)));
        let EventResult::Input(InputEventResult::Transform { text, .. }) = input(&api, "ulw", true).await else { panic!("transform") };
        assert_eq!(text, format!("ulw\n{}", maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));
    }
}

#[tokio::test]
async fn plain_skill_family_mentions_arm_by_upstream_overlap_matching() {
    for prompt in ["ulw-plan", "omo-agent-toolkit ulw-loop status --json"] {
        let api=register();
        let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,prompt,true).await else {panic!("transform")};
        assert_eq!(text,format!("{prompt}\n{}",maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));
    }
}

#[tokio::test]
async fn lone_open_tag_still_arms() {
    let api = register();
    let prompt = "Explain <ultrawork-mode> then ulw fix";
    let EventResult::Input(InputEventResult::Transform { text, .. }) = input(&api, prompt, true).await else { panic!("transform") };
    assert_eq!(text, format!("{prompt}\n{}", maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));
}

#[tokio::test]
async fn re_registration_retains_shared_arming() {
    let arming = Arc::new(Mutex::new(SessionArming::default()));
    let mut before = register();
    before.registered.handlers.clear();
    UltraworkComponent { arming: arming.clone() }.register(&mut before);
    input(&before, "ulw first", true).await;
    let mut after = register();
    after.registered.handlers.clear();
    UltraworkComponent { arming }.register(&mut after);
    let EventResult::Input(InputEventResult::Transform { text, .. }) = input(&after, "ulw resumed", true).await else { panic!("transform") };
    assert_eq!(text, format!("ulw resumed\n{ULTRAWORK_REMINDER}"));
}

#[tokio::test]
async fn accepted_and_rejected_compaction_sequence_keeps_shipped_output() {
    let api = register();
    let mut appended = Vec::new();
    for rejected in [None, None, Some(true), Some(false)] {
        if let Some(rejected) = rejected {
            let compact = if rejected { SessionCompactEvent::Rejected { reason: CompactionReason::Manual,
                request_id: "id".into(), rejection_cause: CompactionRejectionCause::CancelledByExtension }
            } else { SessionCompactEvent::Accepted { reason: CompactionReason::Manual, request_id: "id".into(),
                compaction_entry: SessionEntry { id: "entry".into(), parent_id: None, timestamp: String::new(),
                    kind: "compaction".into(), data: JsonValue::Null }, from_extension: false, will_retry: false } };
            let mut event = ExtensionEvent::SessionCompact(compact);
            api.registered.handlers[&EventKind::SessionCompact][0](&mut event, &support::context()).await.expect("dispatch");
        }
        let EventResult::Input(InputEventResult::Transform { text, .. }) = input(&api, "ulw", true).await else { panic!("transform") };
        appended.push(text);
    }
    let full = format!("ulw\n{}", maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE);
    let reminder = format!("ulw\n{ULTRAWORK_REMINDER}");
    assert_eq!(appended, [full.clone(), reminder.clone(), reminder, full]);
}
