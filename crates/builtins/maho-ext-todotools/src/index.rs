use serde_json::Value;
use crate::todo_types::TodoPhase;
pub struct NativeTodotoolsExtension {pub actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,pub copy_markdown:crate::commands::CopyTodoMarkdown,pub widget_sender:tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>}
struct NativeTodoAccessors {phases:std::sync::Mutex<Vec<TodoPhase>>,ui:std::sync::Mutex<Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>>,widget_sender:tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>}
impl crate::tools_todo::TodoAccessors for NativeTodoAccessors {
    fn get_current_phases(&self)->Vec<TodoPhase>{self.phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()}
    fn set_current_phases(&self,phases:Vec<TodoPhase>){*self.phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=phases;}
    fn sync_widget(&self,_:&dyn maho_ext_api::ToolContext,completed:&[crate::todo_types::TodoCompletionTransition])->Result<(),maho_ext_api::ExtensionFailure>{
        if let Some(ui)=self.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref(){ui.set_widget("todo-sidebar",crate::todo_widget_component::widget_content(&self.phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner),completed,self.widget_sender.clone()),Default::default());}Ok(())
    }
}
impl maho_ext_api::Extension for NativeTodotoolsExtension {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        let accessors=std::sync::Arc::new(NativeTodoAccessors {phases:Default::default(),ui:Default::default(),widget_sender:self.widget_sender.clone()});
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionTree]{let captured=accessors.clone();api.on(event,std::sync::Arc::new(move |_,ctx|{let captured=captured.clone();Box::pin(async move {*captured.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=ctx.has_ui.then(||ctx.ui.clone());Ok(maho_ext_api::EventResult::None)})}));}
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
                let entries=ctx.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
                accessors.set_current_phases(crate::todo_storage::get_latest_phases_from_branch_entries(&entries));
                accessors.sync_widget(ctx,&[])?;
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
    api.on(maho_ext_api::EventKind::MessageEnd,std::sync::Arc::new(move |event,ctx| {
        let actions=actions.clone(); let accessors=accessors.clone();
        Box::pin(async move {
            if let maho_ext_api::ExtensionEvent::MessageEnd{message}=event {
                let message=serde_json::to_value(message).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                for phases in native_todo_updates(&message) {
                    accessors.set_current_phases(phases.clone());
                    actions.append_entry(crate::todo_types::TODO_STATE_ENTRY_TYPE,Some(serde_json::json!({"schema":"v2","phases":phases})))?;
                    accessors.sync_widget(ctx,&[])?;
                }
            }
            Ok(maho_ext_api::EventResult::None)
        })
    }));
}
pub fn register_prompt_hook(api:&mut maho_ext_api::ExtensionApi) {
    api.on(maho_ext_api::EventKind::BeforeAgentStart,std::sync::Arc::new(|event,_ctx|Box::pin(async move {
        Ok(prompt_hook_result(event))
    })));
}
fn prompt_hook_result(event:&maho_ext_api::ExtensionEvent)->maho_ext_api::EventResult {
    match event {
        maho_ext_api::ExtensionEvent::BeforeAgentStart(event)=>maho_ext_api::EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult {
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
            system_prompt_options:maho_ext_api::BuildSystemPromptOptions {cwd:Default::default(),custom_prompt:None,append_system_prompt:None,tools:vec![],skills:vec![],context_files:vec![]},
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
