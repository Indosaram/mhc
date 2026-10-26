use serde_json::Value;
use crate::todo_types::{TodoAsk,TodoPhase,TodoState};
pub struct NativeTodotoolsExtension {pub actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,pub copy_markdown:crate::commands::CopyTodoMarkdown,pub widget_sender:tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>}
struct NativeTodoAccessors {branch:std::sync::Mutex<Vec<Value>>,phases:std::sync::Mutex<Vec<TodoPhase>>,ask:std::sync::Mutex<Option<TodoAsk>>,surface:std::sync::Mutex<maho_ext_api::PromptSurface>,ui:std::sync::Mutex<Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>>,widget_sender:tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>}
impl crate::tools_todo::TodoAccessors for NativeTodoAccessors {
    fn get_current_phases(&self)->Vec<TodoPhase>{self.phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()}
    fn set_current_phases(&self,phases:Vec<TodoPhase>){*self.phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=phases;}
    fn get_current_ask(&self)->Option<TodoAsk>{self.ask.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()}
    fn set_current_ask(&self,ask:Option<TodoAsk>){*self.ask.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=ask;}
    fn get_current_state(&self)->TodoState{TodoState{phases:self.get_current_phases(),ask:self.get_current_ask()}}
    fn branch_entries(&self)->Vec<Value>{self.branch.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()}
    fn prompt_surface(&self)->maho_ext_api::PromptSurface{*self.surface.lock().unwrap_or_else(std::sync::PoisonError::into_inner)}
    fn sync_widget(&self,_:&dyn maho_ext_api::ToolContext,completed:&[crate::todo_types::TodoCompletionTransition])->Result<(),maho_ext_api::ExtensionFailure>{
        if let Some(ui)=self.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref(){ui.set_widget("todo-sidebar",crate::todo_widget_component::widget_content(&self.phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner),completed,self.widget_sender.clone()),Default::default());}Ok(())
    }
}
impl maho_ext_api::Extension for NativeTodotoolsExtension {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        let accessors=std::sync::Arc::new(NativeTodoAccessors {branch:Default::default(),phases:Default::default(),ask:Default::default(),surface:Default::default(),ui:Default::default(),widget_sender:self.widget_sender.clone()});
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionTree]{let captured=accessors.clone();api.on(event,std::sync::Arc::new(move |_,ctx|{let captured=captured.clone();Box::pin(async move {*captured.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=ctx.has_ui.then(||ctx.ui.clone());Ok(maho_ext_api::EventResult::None)})}));}
        {let captured=accessors.clone();api.on(maho_ext_api::EventKind::TurnStart,std::sync::Arc::new(move |_,ctx|{let captured=captured.clone();Box::pin(async move {
            *captured.branch.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=ctx.session_manager.get_branch().into_iter().map(|entry|entry.data).collect();
            *captured.surface.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=ctx.get_system_prompt_options().surface;
            Ok(maho_ext_api::EventResult::None)
        })}));}
        TodotoolsExtension {actions:self.actions.clone(),accessors:accessors.clone(),copy_markdown:self.copy_markdown.clone()}.register(api);
        api.on(maho_ext_api::EventKind::SessionShutdown,std::sync::Arc::new(move |_,ctx|{let accessors=accessors.clone();Box::pin(async move {if ctx.has_ui{ctx.ui.set_widget("todo-sidebar",None,Default::default());}accessors.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();Ok(maho_ext_api::EventResult::None)})}));
    }
}
pub struct TodotoolsExtension {
    pub actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,
    pub accessors:std::sync::Arc<dyn crate::tools_todo::TodoAccessors>,
    pub copy_markdown:crate::commands::CopyTodoMarkdown,
}
impl maho_ext_api::Extension for TodotoolsExtension {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        register_state_hooks(api,self.actions.clone(),self.accessors.clone());
        register_prompt_hook(api);
        register_first_turn_hooks(api);
        crate::tools_todo::register_todo_tool(api,self.actions.clone(),self.accessors.clone());
        crate::commands::register_todo_command(api,self.actions.clone(),self.accessors.clone(),self.copy_markdown.clone());
    }
}
pub fn register_state_hooks(api:&mut maho_ext_api::ExtensionApi,actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,accessors:std::sync::Arc<dyn crate::tools_todo::TodoAccessors>) {
    for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionTree] {
        let accessors=accessors.clone();
        api.on(event,std::sync::Arc::new(move |_event,ctx| {
            let accessors=accessors.clone();
            Box::pin(async move {
                // Each state entry replaces the previous state wholesale, so the newest entry that parses is the whole answer.
                let (_,tail)=ctx.session_manager.get_branch_tail(0,&mut |data:&Value|{
                    let payload=match data.get("type").and_then(Value::as_str) {
                        Some("custom")=>if data.get("customType").and_then(Value::as_str)==Some(crate::todo_types::TODO_STATE_ENTRY_TYPE){data.get("data")}else{None},
                        Some("message")=>data.get("message").filter(|message|message.get("role").and_then(Value::as_str)==Some("toolResult")&&matches!(message.get("toolName").and_then(Value::as_str),Some("todo"|"todowrite"))).and_then(|message|message.get("details")),
                        _=>None,
                    };
                    payload.and_then(crate::todo_storage::read_todo_state_payload).is_some()
                });
                let entries=tail.into_iter().map(|entry|entry.data).collect::<Vec<_>>();
                let state=crate::todo_storage::get_latest_state_from_branch_entries(&entries);
                accessors.set_current_phases(state.phases);
                accessors.set_current_ask(state.ask);
                accessors.sync_widget(ctx,&[])?;
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
    api.on(maho_ext_api::EventKind::MessageEnd,std::sync::Arc::new(move |event,ctx| {
        let actions=actions.clone(); let accessors=accessors.clone();
        Box::pin(async move {
            // Only assistant messages can carry todo tool calls; skip serializing the rest.
            if let maho_ext_api::ExtensionEvent::MessageEnd{message}=event && message.as_assistant().is_some() {
                let message=serde_json::to_value(message).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                for phases in native_todo_updates(&message) {
                    accessors.set_current_phases(phases.clone());
                    actions.append_entry(crate::todo_types::TODO_STATE_ENTRY_TYPE,Some(serde_json::json!({"schema":"v2","phases":phases,"ask":accessors.get_current_ask()})))?;
                    accessors.sync_widget(ctx,&[])?;
                }
            }
            Ok(maho_ext_api::EventResult::None)
        })
    }));
}
pub fn register_prompt_hook(api:&mut maho_ext_api::ExtensionApi) {
    api.on_with_options(maho_ext_api::EventKind::BeforeAgentStart,std::sync::Arc::new(|event,_ctx|Box::pin(async move {
        Ok(prompt_hook_result(event))
    })),maho_ext_api::ExtensionHandlerOptions{preview_safe:true,..Default::default()});
}
/// senpi `pendingForce`: armed by the first-turn opener, cleared when that run's first assistant
/// message lands or the run ends.
#[derive(Clone,Copy)]
struct PendingForce {force_named:bool,logged:bool}
pub fn register_first_turn_hooks(api:&mut maho_ext_api::ExtensionApi) {
    let pending=std::sync::Arc::new(std::sync::Mutex::new(None::<PendingForce>));
    let runtime=api.runtime.clone();
    {let pending=pending.clone();api.on_with_options(maho_ext_api::EventKind::BeforeAgentStart,std::sync::Arc::new(move |event,ctx|{let pending=pending.clone();let runtime=runtime.clone();Box::pin(async move {
        let maho_ext_api::ExtensionEvent::BeforeAgentStart(event)=event else {return Ok(maho_ext_api::EventResult::None)};
        if event.preview {return Ok(maho_ext_api::EventResult::None);}
        // PERF-DIVERGENCE(senpi todotools first-turn): upstream builds the full branch and reads settings before checking the cheap arming gates; we return early on the trigger and mode gates that `should_arm_first_turn` would reject anyway, skipping the O(history) copy and settings read. The result is identical.
        if (event.trigger!="prompt"&&event.trigger!="delivery")||matches!(ctx.mode,maho_ext_api::ExtensionMode::Print|maho_ext_api::ExtensionMode::Json) {return Ok(maho_ext_api::EventResult::None);}
        let branch=ctx.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
        let home=std::env::var("HOME").or_else(|_|std::env::var("USERPROFILE")).unwrap_or_default();
        let setting=maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(),&ctx.agent_dir.to_string_lossy(),&home,ctx.is_project_trusted()).get_todo_first_turn_plan();
        let todo_active=runtime.session_actions().and_then(|actions|actions.get_active_tools()).is_ok_and(|tools|tools.iter().any(|tool|tool=="todo"));
        let armed=crate::first_turn::should_arm_first_turn(&crate::first_turn::FirstTurnInput{preview:event.preview,trigger:&event.trigger,prompt:&event.prompt,branch_entries:&branch,todo_active,setting,mode:ctx.mode});
        if !armed {return Ok(maho_ext_api::EventResult::None);}
        *pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(PendingForce{force_named:setting=="force",logged:false});
        Ok(maho_ext_api::EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult{ system_prompt_parts: None,system_prompt:None,message:Some(maho_ext_api::CustomMessage{custom_type:crate::first_turn::FIRST_TURN_CUSTOM_TYPE.into(),content:vec![maho_ext_api::ToolContent::text(crate::first_turn::FIRST_TURN_REMINDER)],display:false,details:None})}))
    })}),maho_ext_api::ExtensionHandlerOptions{preview_safe:true,..Default::default()});}
    {let pending=pending.clone();api.on(maho_ext_api::EventKind::BeforeProviderRequest,std::sync::Arc::new(move |event,ctx|{let pending=pending.clone();Box::pin(async move {
        let maho_ext_api::ExtensionEvent::BeforeProviderRequest{payload,model,..}=event else {return Ok(maho_ext_api::EventResult::None)};
        let Some(force)=*pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) else {return Ok(maho_ext_api::EventResult::None)};
        let forced=force.force_named.then(||crate::first_turn::with_forced_todo_choice(payload,model.as_ref().or(ctx.model.as_ref()))).flatten();
        if !force.logged {
            if let Some(slot)=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut(){slot.logged=true;}
            let data=serde_json::Map::from_iter([("mode".to_owned(),Value::from(if forced.is_some(){"forced"}else{"reminder-only"}))]);
            maho_core::session_log::SessionLogger::create(Some(&ctx.agent_dir.to_string_lossy()),Default::default()).info("todo_first_turn",Some(&data));
        }
        Ok(forced.map_or(maho_ext_api::EventResult::None,maho_ext_api::EventResult::ProviderPayload))
    })}));}
    for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionTree,maho_ext_api::EventKind::AgentEnd,maho_ext_api::EventKind::SessionAbort,maho_ext_api::EventKind::SessionShutdown,maho_ext_api::EventKind::MessageEnd] {
        let pending=pending.clone();
        api.on(event,std::sync::Arc::new(move |event,_ctx|{let pending=pending.clone();Box::pin(async move {
            let clears=match event {maho_ext_api::ExtensionEvent::MessageEnd{message}=>message.as_assistant().is_some(),_=>true};
            if clears {pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();}
            Ok(maho_ext_api::EventResult::None)
        })}));
    }
}
fn prompt_hook_result(event:&maho_ext_api::ExtensionEvent)->maho_ext_api::EventResult {
    match event {
        maho_ext_api::ExtensionEvent::BeforeAgentStart(event)=>maho_ext_api::EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult { system_prompt_parts: None,
            system_prompt:Some(format!("{}\n{}",event.system_prompt,crate::prompt::TASK_MANAGEMENT_SECTION)),
            message:None,
        }),
        _=>maho_ext_api::EventResult::None,
    }
}
pub fn native_todo_updates(message:&Value)->Vec<Vec<TodoPhase>> {
    if message.get("role").and_then(Value::as_str)!=Some("assistant") { return vec![]; }
    let Some(content)=message.get("content").and_then(Value::as_array) else { return vec![]; };
    content.iter().filter(|block|block.get("type").and_then(Value::as_str)==Some("toolCall") && block.get("name").and_then(Value::as_str)==Some("todo")).filter(|block|!block.get("arguments").and_then(|arguments|arguments.get("op")).is_some_and(js_truthy)).filter_map(|block|crate::native_todo_mirror::phases_from_cursor_todos(block.get("arguments")?.get("todos")?)).collect()
}
fn js_truthy(value:&Value)->bool { match value { Value::Null=>false,Value::Bool(value)=>*value,Value::Number(value)=>value.as_f64().is_some_and(|value|value!=0.0),Value::String(value)=>!value.is_empty(),Value::Array(_)|Value::Object(_)=>true } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn prompt_hook_registers_only_before_agent_start() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("todotools",std::path::PathBuf::new(),Default::default()),Default::default(),Default::default(),Default::default());
        register_prompt_hook(&mut api);
        assert_eq!(api.registered.handlers.len(),1);
        assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::BeforeAgentStart].len(),1);
    }
    #[test] fn prompt_hook_preserves_host_prompt_and_appends_shipped_section() {
        let event=maho_ext_api::ExtensionEvent::BeforeAgentStart(maho_ext_api::BeforeAgentStartEvent {
            prompt:"work".into(),images:None,system_prompt:"host prompt".into(),
            trigger:"prompt".into(),preview:false,
            system_prompt_options:maho_ext_api::BuildSystemPromptOptions {cwd:Default::default(),custom_prompt:None,append_system_prompt:None,tools:vec![],tool_snippets:Default::default(),prompt_guidelines:vec![],skills:vec![],context_files:vec![],surface:Default::default()},
        });
        match prompt_hook_result(&event) {
            maho_ext_api::EventResult::BeforeAgentStart(result)=>{
                assert_eq!(result.system_prompt.unwrap(),format!("host prompt\n{}",crate::prompt::TASK_MANAGEMENT_SECTION));
                assert!(result.message.is_none());
            },
            _=>panic!("expected prompt transformation"),
        }
        assert!(matches!(prompt_hook_result(&maho_ext_api::ExtensionEvent::SessionAbort),maho_ext_api::EventResult::None));
    }
    #[test] fn native_updates_skip_explicit_ops_and_nonassistant_messages() { let message=serde_json::json!({"role":"assistant","content":[{"type":"toolCall","name":"todo","arguments":{"todos":[]}},{"type":"toolCall","name":"todo","arguments":{"op":"view","todos":[]}}]}); assert_eq!(native_todo_updates(&message),vec![vec![]]); assert!(native_todo_updates(&serde_json::json!({"role":"user","content":message["content"]})).is_empty()); }
    #[test] fn empty_op_is_not_an_explicit_operation() { assert_eq!(native_todo_updates(&serde_json::json!({"role":"assistant","content":[{"type":"toolCall","name":"todo","arguments":{"op":"","todos":[]}}]})),vec![vec![]]); }
}
