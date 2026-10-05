// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi.
use crate::todo_types::{TodoOpEntry,TodoOperation};
pub trait TodoAccessors:Send+Sync {
    fn get_current_phases(&self)->Vec<crate::todo_types::TodoPhase>;
    fn set_current_phases(&self,phases:Vec<crate::todo_types::TodoPhase>);
    fn sync_widget(&self,ctx:&dyn maho_tools::definition::ToolContext,completed:&[crate::todo_types::TodoCompletionTransition])->Result<(),maho_ext_api::ExtensionFailure>;
}
pub fn register_todo_tool(api:&mut maho_ext_api::ExtensionApi,actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,accessors:std::sync::Arc<dyn TodoAccessors>) {
    if let Err(error)=api.register_tool_with_renderers(create_todo_tool(actions,accessors),renderers()) {std::panic::panic_any(error);}
}
pub fn renderers()->maho_ext_api::ToolRenderers<(),serde_json::Value> {
    use maho_interactive::theme::ThemeColor;
    use maho_tui::components::text::Text;
    use std::sync::Arc;
    maho_ext_api::ToolRenderers {
        render_call:Some(Arc::new(|args,source,_|{let theme=crate::todo_widget_component::theme(source);let label=render_raw_call_label(args).unwrap_or_else(|error|std::panic::panic_any(error));Box::new(Text::with_padding(theme.fg(ThemeColor::ToolTitle,&theme.bold(&label)),0,0))})),
        render_result:Some(Arc::new(|result,options,source,ctx|{
            let theme=crate::todo_widget_component::theme(source);
            let fallback=result.content.iter().filter_map(|block|match block {maho_ext_api::ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None}).collect::<Vec<_>>().join("\n");
            if ctx.is_error{return Box::new(Text::with_padding(theme.fg(ThemeColor::ToolOutput,&fallback),0,0));}
            let phases:Vec<crate::todo_types::TodoPhase>=result.details.get("phases").map(|value|serde_json::from_value(value.clone()).unwrap_or_else(|error|std::panic::panic_any(error))).unwrap_or_default();
            let completed:Vec<crate::todo_types::TodoCompletionTransition>=result.details.get("completedTasks").map(|value|serde_json::from_value(value.clone()).unwrap_or_else(|error|std::panic::panic_any(error))).unwrap_or_default();
            let operation=result.details.get("op").map(|value|serde_json::from_value(value.clone()).unwrap_or_else(|error|std::panic::panic_any(error)));
            let visible:Vec<_>=phases.into_iter().filter(|phase|!phase.tasks.is_empty()).collect();
            let touched=if options.expanded||visible.len()==1{None}else{compute_touched_phases(&ctx.args,operation,&visible,&completed)};
            let mut rows=vec![];
            for (index,phase) in visible.iter().enumerate(){
                let heading=format!("{}. {}",phase_roman_numeral(index+1),crate::todo_format::sanitize_todo_text(&phase.name));
                if touched.as_ref().is_some_and(|set|!set.contains(&phase.name)){let closed=phase.tasks.iter().filter(|task|matches!(task.status,crate::todo_types::TodoStatus::Completed|crate::todo_types::TodoStatus::Abandoned)).count();rows.push(theme.fg(ThemeColor::Dim,&format!("{heading} — {closed}/{} done",phase.tasks.len())));continue;}
                rows.push(theme.fg(ThemeColor::Accent,&theme.bold(&heading)));
                rows.extend(phase.tasks.iter().map(|task|format!("  {}",crate::todo_widget_component::format_task(task,&theme,completed.iter().any(|transition|transition.phase==phase.name&&transition.content==task.content),ctx.spinner_frame.and_then(|frame|i64::try_from(frame).ok())))));
            }
            let text=if !rows.is_empty(){rows.join("\n")}else if !fallback.is_empty(){fallback}else{"Todo list is empty.".into()};
            Box::new(Text::with_padding(text,0,0))
        })),
    }
}
pub fn create_todo_tool(actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,accessors:std::sync::Arc<dyn TodoAccessors>)->maho_tools::definition::ToolDefinition {
    use maho_tools::definition::{ToolDefinition,ToolError,ToolResult,ToolContent,ToolExecutionMode};
    let mut tool=ToolDefinition::new("todo",crate::prompt::TODO_TOOL_DESCRIPTION,parameters(),std::sync::Arc::new(move |call| {
        let actions=actions.clone(); let accessors=accessors.clone();
        Box::pin(async move {
            let ctx=call.context.ok_or_else(||ToolError::Message("todo requires extension execution context".into()))?;
            let storage=if ctx.session_manager().session_file().is_some() { crate::todo_types::TodoStorage::Session } else { crate::todo_types::TodoStorage::Memory };
            let previous=accessors.get_current_phases();
            let (text,details)=plan_execution(&call.params,&previous,storage).map_err(ToolError::Message)?;
            if details.op!=Some(TodoOperation::View) {
                let entry=crate::todo_types::TodoStateEntry{schema:crate::todo_types::TodoStateSchema::V2,phases:details.phases.clone()};
                actions.append_entry(crate::todo_types::TODO_STATE_ENTRY_TYPE,Some(serde_json::to_value(entry)?)).map_err(|error|ToolError::Message(error.to_string()))?;
                accessors.set_current_phases(details.phases.clone());
                accessors.sync_widget(ctx,details.completed_tasks.as_deref().unwrap_or_default()).map_err(|error|ToolError::Message(error.to_string()))?;
            }
            Ok(ToolResult{content:vec![ToolContent::text(text)],details:Some(serde_json::to_value(details)?)})
        })
    }));
    tool.label="Todo".into(); tool.execution_mode=Some(ToolExecutionMode::Sequential);
    tool.prompt_snippet=Some("Track phased tasks with one op-based todo tool; reference tasks by their exact content.".into());
    tool.prompt_guidelines=Some(vec!["Use one todo operation at a time; batch it with the real work rather than making a solo todo turn.".into(),"Reference tasks and phases by their exact content/name; use view when the text is uncertain.".into(),"Mark work done immediately and use drop for tasks that are no longer needed.".into()]); tool
}
pub fn parameters()->serde_json::Value {
    serde_json::json!({"type":"object","properties":{
        "op":{"anyOf":[{"const":"init","type":"string"},{"const":"start","type":"string"},{"const":"done","type":"string"},{"const":"rm","type":"string"},{"const":"drop","type":"string"},{"const":"append","type":"string"},{"const":"view","type":"string"}],"description":"Operation to perform. Required — always pass it explicitly."},
        "list":{"type":"array","description":"Phased task list for init","items":{"type":"object","properties":{"phase":{"type":"string","description":"Phase name"},"items":{"type":"array","items":{"type":"string","description":"Task content"},"description":"Tasks for this phase","minItems":1}},"required":["phase","items"]}},
        "task":{"type":"string","description":"Exact task text copied from the previous todo result"},"phase":{"type":"string","description":"Exact phase name copied from the previous todo result"},
        "items":{"type":"array","items":{"type":"string","description":"Task content"},"description":"Task texts to append"}
    }})
}
pub fn phase_roman_numeral(mut index:usize)->String {
    let mut output=String::new();
    for (value,symbol) in [(1000,"M"),(900,"CM"),(500,"D"),(400,"CD"),(100,"C"),(90,"XC"),(50,"L"),(40,"XL"),(10,"X"),(9,"IX"),(5,"V"),(4,"IV"),(1,"I")] { while index>=value { output.push_str(symbol); index-=value; } } output
}
pub fn count_init_items(params:&TodoOpEntry)->(usize,usize) {
    if let Some(list)=&params.list { return (list.len(),list.iter().map(|phase|phase.items.len()).sum()); }
    params.items.as_ref().map_or((0,0),|items|(usize::from(!items.is_empty()),items.len()))
}
fn count_label(count:usize,singular:&str)->String { format!("{count} {singular}{}",if count==1 { "" } else { "s" }) }
pub fn compute_touched_phases(args:&serde_json::Value,operation:Option<TodoOperation>,phases:&[crate::todo_types::TodoPhase],completed:&[crate::todo_types::TodoCompletionTransition])->Option<std::collections::BTreeSet<String>> {
    let mut touched=std::collections::BTreeSet::new();
    if let Some(active)=crate::todo_query::next_actionable_task(phases) && let Some(phase)=phases.iter().find(|phase|phase.tasks.iter().any(|task|std::ptr::eq(task,active))) { touched.insert(phase.name.clone()); }
    touched.extend(completed.iter().map(|transition|transition.phase.clone()));
    if operation==Some(TodoOperation::Init) { touched.extend(phases.iter().map(|phase|phase.name.clone())); }
    else {
        if let Some(name)=args.get("phase").and_then(serde_json::Value::as_str) && let Some(phase)=phases.iter().find(|phase|phase.name==name) { touched.insert(phase.name.clone()); }
        if let Some(content)=args.get("task").and_then(serde_json::Value::as_str) && let Some((phase,_))=crate::todo_query::find_task_by_content(phases,content) { touched.insert(phases[phase].name.clone()); }
    }
    (!touched.is_empty()).then_some(touched)
}
pub fn plan_execution(raw:&serde_json::Value,previous:&[crate::todo_types::TodoPhase],storage:crate::todo_types::TodoStorage)->Result<(String,crate::todo_types::TodoToolDetails),String> {
    let normalized=crate::normalize::normalize_todo_params(raw,previous);
    let entry=match (normalized.error,normalized.entry) { (Some(error),_)=>return Err(format!("{error}\n\n{}",crate::todo_format::format_summary(previous,&[],true))),(_,Some(entry))=>entry,_=>return Err(format!("Missing \"op\". Example: {{\"op\":\"init\",\"list\":[{{\"phase\":\"Setup\",\"items\":[\"...\"]}}]}}\n\n{}",crate::todo_format::format_summary(previous,&[],true))) };
    let mut corrections=normalized.corrections; let read_only=entry.op==TodoOperation::View;
    let applied=crate::todo_operations::apply_params(previous.to_vec(),&entry,Some(&mut corrections));
    if !applied.errors.is_empty() { return Err(crate::todo_format::format_summary(previous,&applied.errors,read_only)); }
    let completed=if read_only { vec![] } else { crate::todo_query::get_completion_transitions(previous,&applied.phases) };
    let summary=crate::todo_format::format_summary(&applied.phases,&[],read_only);
    let text=if corrections.is_empty() { summary } else { format!("{}\n\n{summary}",corrections.join("\n")) };
    Ok((text,crate::todo_types::TodoToolDetails{op:Some(entry.op),phases:applied.phases,storage,corrections:if corrections.is_empty() { None } else { Some(corrections) },completed_tasks:if completed.is_empty() { None } else { Some(completed) }}))
}
pub fn render_raw_call_label(params:&serde_json::Value)->Result<String,serde_json::Error> {
    if params.get("op").is_none() { return Ok("todo".into()); }
    let entry=serde_json::from_value::<TodoOpEntry>(params.clone())?;
    Ok(render_call_label(&entry))
}
pub fn render_call_label(params:&TodoOpEntry)->String {
    let clean=|value:&str|crate::todo_format::sanitize_todo_text(value);
    match params.op {
        TodoOperation::Init=>{ let (phases,tasks)=count_init_items(params); format!("todo init ({}, {})",count_label(phases,"phase"),count_label(tasks,"task")) },
        TodoOperation::Append=>{ let phase=clean(params.phase.as_deref().unwrap_or("")); format!("todo append: {} ({})",if phase.is_empty() { "(missing phase)" } else { &phase },count_label(params.items.as_ref().map_or(0,Vec::len),"item")) },
        TodoOperation::Start|TodoOperation::Done|TodoOperation::Drop=>{ let target=clean(params.task.as_deref().or(params.phase.as_deref()).unwrap_or("")); let op=match params.op { TodoOperation::Start=>"start",TodoOperation::Done=>"done",TodoOperation::Drop=>"drop",_=>unreachable!() }; format!("todo {op}: {}",if target.is_empty() { "(missing target)" } else { &target }) },
        TodoOperation::Rm=>{ let target=clean(params.task.as_deref().or(params.phase.as_deref()).unwrap_or("all")); format!("todo rm: {}",if target.is_empty() { "all" } else { &target }) },TodoOperation::View=>"todo view".into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_result_renderer_collapses_untouched_phases_and_expands_them() {
        use maho_ext_api::{AgentToolResult,ToolRenderContext,ToolRendererSession};
        for width in [40,80,120] {
            let context=ToolRenderContext {args:serde_json::json!({"op":"done","task":"Finished"}),tool_call_id:"todo-render".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:false,expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:Some(8)};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();let theme=maho_ext_api::Theme::default();let mut states=vec![slots.render_call(&theme,width).unwrap()];
            let mut result=AgentToolResult::text("");result.details=serde_json::json!({"op":"done","phases":[{"name":"Delivery","tasks":[{"content":"Finished","status":"completed"},{"content":"Active","status":"in_progress"}]},{"name":"Future","tasks":[{"content":"Hidden future task","status":"pending"}]}],"completedTasks":[{"phase":"Delivery","content":"Finished"}]});
            states.push(slots.render_result(&result,&theme,width).unwrap());assert!(!states.last().unwrap().join("\n").contains("Hidden future task"));
            slots.session.context.expanded=true;states.push(slots.render_result(&result,&theme,width).unwrap());assert!(states.last().unwrap().join("\n").contains("Hidden future task"));
            for lines in &states {assert!(lines.iter().all(|line|maho_tui::utils::visible_width(line)<=width));}
            println!("TODO_RENDER_JSON={}",serde_json::json!({"width":width,"states":states}));
        }
    }
    #[derive(Default)]
    struct ExecutionFixture { phases:std::sync::Mutex<Vec<crate::todo_types::TodoPhase>>,events:std::sync::Mutex<Vec<&'static str>>,entries:std::sync::Mutex<Vec<serde_json::Value>>,fail_append:bool }
    impl TodoAccessors for ExecutionFixture {
        fn get_current_phases(&self)->Vec<crate::todo_types::TodoPhase> { self.phases.lock().unwrap().clone() }
        fn set_current_phases(&self,phases:Vec<crate::todo_types::TodoPhase>) { self.events.lock().unwrap().push("set"); *self.phases.lock().unwrap()=phases; }
        fn sync_widget(&self,_ctx:&dyn maho_tools::definition::ToolContext,_completed:&[crate::todo_types::TodoCompletionTransition])->Result<(),maho_ext_api::ExtensionFailure> { self.events.lock().unwrap().push("widget"); Ok(()) }
    }
    impl maho_ext_api::ExtensionActions for ExecutionFixture {
        fn send_message(&self,message:maho_ext_api::CustomMessage,options:maho_ext_api::SendMessageOptions)->Result<(),maho_ext_api::ExtensionFailure> { assert_eq!(message.custom_type,"todotools.user-edit"); assert!(!message.display); assert!(!options.trigger_turn); assert_eq!(options.deliver_as,Some(maho_ext_api::DeliverAs::NextTurn)); self.events.lock().unwrap().push("message"); Ok(()) }
        fn send_user_message(&self,_content:maho_ext_api::UserMessageContent,_options:maho_ext_api::SendUserMessageOptions)->Result<(),maho_ext_api::ExtensionFailure> { panic!("not used") }
        fn append_entry(&self,kind:&str,data:Option<serde_json::Value>)->Result<(),maho_ext_api::ExtensionFailure> { assert_eq!(kind,crate::todo_types::TODO_STATE_ENTRY_TYPE); if self.fail_append { return Err(maho_ext_api::ExtensionFailure::new("append failed")); } self.events.lock().unwrap().push("append"); self.entries.lock().unwrap().push(data.unwrap()); Ok(()) }
        fn get_all_tools(&self)->Result<Vec<maho_ext_api::ToolInfo>,maho_ext_api::ExtensionFailure> { panic!("not used") }
    }
    struct Context { persisted:bool }
    impl maho_ext_api::SessionManager for Context {
        fn get_entries(&self)->Vec<maho_ext_api::SessionEntry> {vec![]}
        fn get_branch(&self)->Vec<maho_ext_api::SessionEntry> {vec![]}
        fn get_leaf_id(&self)->Option<String> {None}
        fn get_session_name(&self)->Option<String> {None}
    }
    struct Registry;
    impl maho_ext_api::ModelRegistry for Registry {
        fn get_all(&self)->Vec<maho_ext_api::Model> {vec![]}
        fn get_available(&self)->Vec<maho_ext_api::Model> {vec![]}
        fn find(&self,_provider:&str,_id:&str)->Option<maho_ext_api::Model> {None}
        fn has_configured_auth(&self,_model:&maho_ext_api::Model)->bool {false}
        fn get_api_key_for_provider<'a>(&'a self,_provider:&'a str)->maho_ext_api::ExtensionFuture<'a,Option<String>> {panic!("commands do not read credentials")}
    }
    #[derive(Default)] struct CommandUi(std::sync::Mutex<Vec<maho_ext_api::NotificationType>>,std::sync::Mutex<Vec<(String,Option<maho_ext_api::WidgetContent>)>>);
    impl maho_ext_api::ExtensionUi for CommandUi {
        fn select<'a>(&'a self,_title:&'a str,_options:&'a [String],_opts:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>> {panic!("not used")}
        fn confirm<'a>(&'a self,_title:&'a str,_message:&'a str,_opts:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,bool> {panic!("not used")}
        fn input<'a>(&'a self,_title:&'a str,_placeholder:Option<&'a str>,_opts:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>> {panic!("not used")}
        fn notify(&self,_message:&str,kind:maho_ext_api::NotificationType) {self.0.lock().unwrap().push(kind);}
        fn set_status(&self,_key:&str,_text:Option<&str>) {panic!("not used")}
        fn set_widget(&self,key:&str,content:Option<maho_ext_api::WidgetContent>,_options:maho_ext_api::ExtensionWidgetOptions) {self.1.lock().unwrap().push((key.into(),content));}
        fn set_header(&self,_factory:Option<maho_ext_api::ComponentFactory>) {panic!("not used")}
        fn set_footer(&self,_factory:Option<maho_ext_api::ComponentFactory>) {panic!("not used")}
        fn set_title(&self,_title:&str) {panic!("not used")}
        fn paste_to_editor(&self,_text:&str) {panic!("not used")}
        fn set_editor_text(&self,_text:&str) {panic!("not used")}
        fn get_editor_text(&self)->String {panic!("not used")}
        fn custom(&self,_factory:maho_ext_api::ComponentFactory,_options:maho_ext_api::CustomUiOptions)->maho_ext_api::ExtensionFuture<'_,serde_json::Value> {panic!("not used")}
        fn theme(&self)->maho_ext_api::Theme {panic!("not used")}
    }
    fn command_context(ui:std::sync::Arc<CommandUi>,cwd:std::path::PathBuf)->maho_ext_api::ExtensionContext {
        maho_ext_api::ExtensionContext{ui,mode:Default::default(),has_ui:true,cwd,agent_dir:Default::default(),session_manager:std::sync::Arc::new(Context{persisted:true}),model_registry:std::sync::Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:vec![],goal_store_file:None,loaded_extension_paths:vec![],signal:None,steering_signal:None,is_idle_fn:std::sync::Arc::new(||true),wait_for_idle_fn:std::sync::Arc::new(||Box::pin(async {})),is_project_trusted_fn:std::sync::Arc::new(||true),is_compacting_fn:std::sync::Arc::new(||false),get_system_prompt_fn:std::sync::Arc::new(String::new),get_system_prompt_options_fn:std::sync::Arc::new(Default::default),registered_mcp_servers:vec![],update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default()}
    }
    #[tokio::test] async fn registered_command_executes_mutations_copy_and_file_roundtrip() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default()); let copied=std::sync::Arc::new(std::sync::Mutex::new(vec![])); let captured=copied.clone();
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("todotools",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        crate::commands::register_todo_command(&mut api,fixture.clone(),fixture.clone(),std::sync::Arc::new(move |text| {captured.lock().unwrap().push(text);Box::pin(async {Ok(())})}));
        let temp=tempfile::tempdir().unwrap(); let ui=std::sync::Arc::new(CommandUi::default()); let ctx=command_context(ui.clone(),temp.path().into()); let handler=&api.registered.commands[0].handler;
        handler("append Work",&ctx).await.unwrap();
        assert_eq!(fixture.get_current_phases()[0].tasks[0].content,"Work");
        handler("copy",&ctx).await.unwrap(); assert_eq!(copied.lock().unwrap()[0],crate::markdown::phases_to_markdown(&fixture.get_current_phases()));
        handler("export",&ctx).await.unwrap(); assert!(temp.path().join("TODO.md").is_file());
        handler("rm",&ctx).await.unwrap(); assert!(fixture.get_current_phases().is_empty());
        handler("import",&ctx).await.unwrap(); assert_eq!(fixture.get_current_phases()[0].tasks[0].content,"Work");
        let count=fixture.entries.lock().unwrap().len(); handler("start missing",&ctx).await.unwrap(); assert_eq!(fixture.entries.lock().unwrap().len(),count);
        assert_eq!(*ui.0.lock().unwrap().last().unwrap(),maho_ext_api::NotificationType::Error);
        assert_eq!(fixture.events.lock().unwrap().iter().filter(|event|**event=="message").count(),3);
    }
    #[tokio::test] async fn native_factory_captures_ui_mounts_component_and_clears_on_shutdown() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default());
        let ui=std::sync::Arc::new(CommandUi::default());
        let ctx=command_context(ui.clone(),Default::default());
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("todotools",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        maho_ext_api::Extension::register(&crate::index::NativeTodotoolsExtension {actions:fixture,copy_markdown:std::sync::Arc::new(|_|Box::pin(async {panic!("not used")})),widget_sender:tokio::sync::mpsc::unbounded_channel().0},&mut api);
        let mut tree=maho_ext_api::ExtensionEvent::SessionTree{new_leaf_id:None,old_leaf_id:None,summary_entry:None,from_extension:None};
        for handler in &api.registered.handlers[&maho_ext_api::EventKind::SessionTree] {handler(&mut tree,&ctx).await.unwrap();}
        assert!(ui.1.lock().unwrap()[0].1.is_none());
        (api.registered.commands[0].handler)("append \"Native widget task\"",&ctx).await.unwrap();
        {
            let widgets=ui.1.lock().unwrap();
            let (key,content)=widgets.last().unwrap();
            assert_eq!(key,"todo-sidebar");
            let Some(maho_ext_api::WidgetContent::Component(factory))=content else {panic!("expected native component")};
            let mut component=factory(&Default::default());
            let rendered=component.render(40).join("\n");
            assert!(rendered.contains("Native widget task"),"mounted widget: {rendered:?}");
        }
        let mut shutdown=maho_ext_api::ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None});
        (api.registered.handlers[&maho_ext_api::EventKind::SessionShutdown][0])(&mut shutdown,&ctx).await.unwrap();
        let widgets=ui.1.lock().unwrap();
        assert_eq!(widgets.last().unwrap().0,"todo-sidebar");
        assert!(widgets.last().unwrap().1.is_none());
    }
    impl maho_ext_api::ToolSessionManager for Context {
        fn session_id(&self)->&str { "test" }
        fn session_file(&self)->Option<&std::path::Path> { self.persisted.then(||std::path::Path::new("/session.jsonl")) }
    }
    impl maho_ext_api::ToolContext for Context {
        fn cwd(&self)->&std::path::Path { std::path::Path::new("/tmp") }
        fn model(&self)->Option<&maho_ext_api::Model> { None }
        fn thinking_level(&self)->Option<maho_ext_api::ThinkingLevel> { None }
        fn session_manager(&self)->&dyn maho_ext_api::ToolSessionManager { self }
        fn goal_store_file(&self)->Option<&std::path::Path> { None }
    }
    #[test] fn command_commit_sets_state_before_persistence_and_hidden_reminder() {
        let fixture=ExecutionFixture::default(); let mutation=crate::commands::status_command(&[],"",crate::todo_types::TodoOperation::Rm).unwrap();
        crate::commands::commit_command_mutation(&Context{persisted:true},&mutation,&fixture,&fixture).unwrap();
        assert_eq!(*fixture.events.lock().unwrap(),["set","append","widget","message"]);
        assert_eq!(fixture.entries.lock().unwrap()[0]["source"],"user");
        assert_eq!(fixture.entries.lock().unwrap()[0]["action"],"/todo rm (all)");
        let failed=ExecutionFixture{fail_append:true,..Default::default()};
        assert!(crate::commands::commit_command_mutation(&Context{persisted:true},&mutation,&failed,&failed).is_err());
        assert_eq!(*failed.events.lock().unwrap(),["set"]);
    }
    #[tokio::test] async fn native_execution_persists_before_state_and_widget() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default()); let tool=create_todo_tool(fixture.clone(),fixture.clone());
        let result=(tool.execute)(maho_ext_api::ToolCall{id:"todo",params:serde_json::json!({"op":"init","items":["Work"]}),signal:Default::default(),on_update:None,context:Some(&Context{persisted:true})}).await.unwrap();
        assert_eq!(*fixture.events.lock().unwrap(),["append","set","widget"]);
        assert_eq!(fixture.entries.lock().unwrap()[0]["schema"],"v2");
        assert_eq!(result.details.as_ref().unwrap()["storage"],"session");
        assert_eq!(fixture.get_current_phases()[0].tasks[0].status,crate::todo_types::TodoStatus::InProgress);
    }
    #[test] fn state_hooks_register_session_tree_and_native_mirror_events() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default());
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("todotools",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        crate::index::register_state_hooks(&mut api,fixture.clone(),fixture.clone());
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionTree,maho_ext_api::EventKind::MessageEnd] { assert_eq!(api.registered.handlers[&event].len(),1); }
        assert!(api.registered.tools.is_empty());
        register_todo_tool(&mut api,fixture.clone(),fixture.clone());
        assert_eq!(api.registered.tools.len(),1);
        assert_eq!(api.registered.tools[0].definition.name,"todo");
        assert_eq!(api.registered.tools[0].definition.execution_mode,Some(maho_ext_api::ToolExecutionMode::Sequential));
        crate::commands::register_todo_command(&mut api,fixture.clone(),fixture,std::sync::Arc::new(|_|Box::pin(async {panic!("clipboard must only run on copy")})));
        assert_eq!(api.registered.commands.len(),1);
        assert_eq!(api.registered.commands[0].name,"todo");
    }
    #[test] fn extension_composes_native_tool_command_and_source_event_hooks() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default());
        let extension=crate::index::TodotoolsExtension{actions:fixture.clone(),accessors:fixture,copy_markdown:std::sync::Arc::new(|_|Box::pin(async {panic!("registration cannot access clipboard")}))};
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("todotools",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        maho_ext_api::Extension::register(&extension,&mut api);
        assert_eq!(api.registered.tools.len(),1);assert_eq!(api.registered.commands.len(),1);
        assert_eq!(api.registered.handlers.len(),4);
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::SessionTree,maho_ext_api::EventKind::MessageEnd,maho_ext_api::EventKind::BeforeAgentStart] {assert_eq!(api.registered.handlers[&event].len(),1);}
    }
    #[tokio::test] async fn registered_state_hooks_reload_and_mirror_in_source_order() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default());
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("todotools",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        crate::index::register_state_hooks(&mut api,fixture.clone(),fixture.clone());
        let ctx=command_context(std::sync::Arc::new(CommandUi::default()),Default::default());
        let mut tree=maho_ext_api::ExtensionEvent::SessionTree{new_leaf_id:None,old_leaf_id:None,summary_entry:None,from_extension:None};
        (api.registered.handlers[&maho_ext_api::EventKind::SessionTree][0])(&mut tree,&ctx).await.unwrap();
        assert_eq!(*fixture.events.lock().unwrap(),["set","widget"]);
        fixture.events.lock().unwrap().clear();
        let message=serde_json::from_value(serde_json::json!({"role":"assistant","content":[{"type":"toolCall","id":"native","name":"todo","arguments":{"todos":[{"content":" Native task ","status":"in_progress"}]}}],"api":"test","provider":"test","model":"test","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":0})).unwrap();
        let mut event=maho_ext_api::ExtensionEvent::MessageEnd{message};
        (api.registered.handlers[&maho_ext_api::EventKind::MessageEnd][0])(&mut event,&ctx).await.unwrap();
        assert_eq!(*fixture.events.lock().unwrap(),["set","append","widget"]);
        assert_eq!(fixture.get_current_phases()[0].tasks[0].content,"Native task");
        assert_eq!(fixture.entries.lock().unwrap()[0]["schema"],"v2");
    }
    #[tokio::test] async fn native_view_and_invalid_operations_do_not_mutate() {
        let fixture=std::sync::Arc::new(ExecutionFixture::default()); let tool=create_todo_tool(fixture.clone(),fixture.clone());
        let viewed=(tool.execute)(maho_ext_api::ToolCall{id:"view",params:serde_json::json!({"op":"view"}),signal:Default::default(),on_update:None,context:Some(&Context{persisted:false})}).await.unwrap();
        assert_eq!(viewed.details.unwrap()["storage"],"memory");
        assert!((tool.execute)(maho_ext_api::ToolCall{id:"invalid",params:serde_json::json!({"op":"done","task":"unknown"}),signal:Default::default(),on_update:None,context:Some(&Context{persisted:false})}).await.is_err());
        assert!(fixture.events.lock().unwrap().is_empty());
    }
    #[tokio::test] async fn failed_persistence_does_not_change_current_state() {
        let fixture=std::sync::Arc::new(ExecutionFixture{fail_append:true,..Default::default()}); let tool=create_todo_tool(fixture.clone(),fixture.clone());
        assert!((tool.execute)(maho_ext_api::ToolCall{id:"todo",params:serde_json::json!({"op":"init","items":["Work"]}),signal:Default::default(),on_update:None,context:Some(&Context{persisted:true})}).await.is_err());
        assert!(fixture.get_current_phases().is_empty()); assert!(fixture.events.lock().unwrap().is_empty());
    }
    #[test] fn touched_phases_include_active_completed_and_exact_target() {
        use crate::todo_types::{TodoPhase,TodoItem,TodoStatus,TodoCompletionTransition};
        let phases:Vec<_>=[("Active",TodoStatus::InProgress),("Closed",TodoStatus::Completed),("Target",TodoStatus::Pending)].into_iter().map(|(name,status)|TodoPhase{name:name.into(),tasks:vec![TodoItem{content:name.into(),status}]}).collect();
        let touched=compute_touched_phases(&serde_json::json!({"task":"Target"}),Some(TodoOperation::Done),&phases,&[TodoCompletionTransition{phase:"Closed".into(),content:"Closed".into()}]).unwrap(); assert_eq!(touched.len(),3); assert!(touched.contains("Target"));
        assert_eq!(compute_touched_phases(&serde_json::json!({"task":"target"}),Some(TodoOperation::View),&phases,&[]).unwrap().into_iter().collect::<Vec<_>>(),["Active"]);
    }
    #[test] fn empty_touched_set_means_all_phases() { assert_eq!(compute_touched_phases(&serde_json::json!({}),None,&[],&[]),None); }
    #[test] fn schema_preserves_optional_op_and_unconstrained_items() { let schema=parameters(); assert!(schema.get("required").is_none()); assert!(schema["properties"]["items"].get("minItems").is_none()); assert_eq!(schema["properties"]["list"]["items"]["properties"]["items"]["minItems"],1); }
    #[test] fn planned_done_includes_completion_transition() { let previous=vec![crate::todo_types::TodoPhase{name:"Setup".into(),tasks:vec![crate::todo_types::TodoItem{content:"Task".into(),status:crate::todo_types::TodoStatus::InProgress}]}]; let (_,details)=plan_execution(&serde_json::json!({"op":"done","task":"Task"}),&previous,crate::todo_types::TodoStorage::Memory).unwrap(); assert_eq!(details.completed_tasks.unwrap()[0].content,"Task"); assert_eq!(previous[0].tasks[0].status,crate::todo_types::TodoStatus::InProgress); }
    #[test] fn planned_view_keeps_current_state() { let (_,details)=plan_execution(&serde_json::json!({"op":"view"}),&[],crate::todo_types::TodoStorage::Session).unwrap(); assert_eq!(details.completed_tasks,None); assert_eq!(details.storage,crate::todo_types::TodoStorage::Session); }
    #[test] fn omitted_operation_renders_before_execute_inference() {
        let raw=serde_json::json!({"items":["Work"]});
        assert_eq!(render_raw_call_label(&raw).unwrap(),"todo");
        let (_,details)=plan_execution(&raw,&[],crate::todo_types::TodoStorage::Memory).unwrap();
        assert_eq!(details.op,Some(TodoOperation::Init));
    }
    #[test] fn raw_call_keeps_empty_target_and_list_precedence() {
        assert_eq!(render_raw_call_label(&serde_json::json!({"op":"done","task":"","phase":"Setup"})).unwrap(),"todo done: (missing target)");
        assert_eq!(render_raw_call_label(&serde_json::json!({"op":"init","list":[],"items":["Work"]})).unwrap(),"todo init (0 phases, 0 tasks)");
        assert!(render_raw_call_label(&serde_json::json!({"op":"unknown"})).is_err());
    }
    #[test] fn roman_numerals_cover_subtractive_pairs() { assert_eq!(phase_roman_numeral(0),""); assert_eq!(phase_roman_numeral(1994),"MCMXCIV"); assert_eq!(phase_roman_numeral(49),"XLIX"); }
    #[test] fn phased_init_counts_each_task() { let params=TodoOpEntry{op:TodoOperation::Init,list:Some(vec![crate::todo_types::TodoPhaseInput{phase:"One".into(),items:vec!["a".into(),"b".into()]}]),task:None,phase:None,items:Some(vec!["ignored".into()])}; assert_eq!(count_init_items(&params),(1,2)); assert_eq!(render_call_label(&params),"todo init (1 phase, 2 tasks)"); }
}

#[cfg(test)]
mod exported_prefix_tests {
    use super::*;
    use maho_ext_api::{ToolRenderContext,ToolRendererSession,AgentToolResult};
    use maho_interactive::theme::{Theme,ThemeColor,ThemeBg,theme::ColorMode};
    use serde_json::json;
    #[test]
    fn callbacks_preserve_current_exported_prefix_bytes() {
        for mode in [ColorMode::Color256,ColorMode::Truecolor] {
            let native=Theme::builtin("dark",mode).unwrap();
            let exported=maho_ext_api::Theme {name:Some(native.name.clone()),colors:ThemeColor::ALL.iter().map(|color|(color.key().into(),native.get_fg_ansi(*color))).collect(),backgrounds:ThemeBg::ALL.iter().map(|bg|(bg.key().into(),native.get_bg_ansi(*bg))).collect(),vars:Default::default()};
            let context=ToolRenderContext {args:json!({"op":"view"}),tool_call_id:"prefix-proof".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:Default::default(),execution_started:false,args_complete:true,is_partial:false,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None};
            let mut slots=ToolRendererSession {renderers:std::sync::Arc::new(renderers()),context}.into_slots();
            let call=slots.render_call(&exported,80).unwrap().join("
");
            assert!(call.contains(&native.get_fg_ansi(ThemeColor::ToolTitle)));
            let result=slots.render_result(&AgentToolResult::text("native output"),&exported,80).unwrap().join("
");
            for output in [&call,&result] {assert!(!output.is_empty());if mode==ColorMode::Color256 {assert!(!output.contains("\x1b[38;2;"));assert!(!output.contains("\x1b[48;2;"));}}
            println!("CONSUMER_PREFIX_JSON={}",json!({"consumer":"maho-ext-todotools/src/tools_todo.rs","mode":format!("{mode:?}"),"call":call,"result":result}));
        }
    }
}
