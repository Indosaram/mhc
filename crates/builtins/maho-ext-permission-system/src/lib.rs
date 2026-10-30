pub mod types;
pub mod wildcard;
pub mod evaluate;
pub mod config;
pub mod cli;
pub mod arity;
pub mod storage;
pub mod events;
pub mod service;
pub mod non_interactive;
pub mod settings;
pub mod prompt;
pub mod external_dir;
pub mod parsers;
pub mod bash_command_segments;
pub mod external_pattern;
pub mod external_spellings;

use maho_ext_api::{Extension,ExtensionApi,EventKind,ExtensionEvent,EventResult,FlagType,FlagValue,ExtensionFailure,ToolCallEventResult};
use std::sync::{Arc,Mutex};
struct SessionPolicy{service:service::PermissionService,registry:parsers::ParserRegistry,preset_bound:bool,initial_approved:usize}
pub struct PermissionSystem;
impl Extension for PermissionSystem{
    fn register(&self,api:&mut ExtensionApi){
        api.register_flag("permission",FlagType::String{default:None},Some("Set permission rules (format: tool=action or tool:pattern=action)".into()));
        api.register_flag("permission-preset",FlagType::String{default:None},Some("Set permission preset (guarded, full-access, workspace, accept-edits, auto, read-only, or ask)".into()));
        let state:Arc<Mutex<Option<SessionPolicy>>>=Arc::new(Mutex::new(None));
        let counter=Arc::new(std::sync::atomic::AtomicU64::new(0));
        let start_state=Arc::clone(&state);let runtime=api.runtime.clone();let bus=api.events.clone();
        api.on(EventKind::SessionStart,Arc::new(move|_,ctx|{
            let state=Arc::clone(&start_state);let runtime=runtime.clone();let bus=bus.clone();
            Box::pin(async move{
                let home=std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let manager=maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(),&ctx.agent_dir.to_string_lossy(),&home,ctx.is_project_trusted());
                let cli=match runtime.get_flag("permission"){Some(FlagValue::String(value))=>cli::parse_permission_flag(&value),_=>Vec::new()};
                let preset=match runtime.get_flag("permission-preset"){
                    Some(FlagValue::String(value))=>Some(cli::parse_permission_preset_flag(&value).ok_or_else(||ExtensionFailure::new(format!("Invalid --permission-preset \"{value}\". Expected one of: guarded, full-access, workspace, accept-edits, auto, read-only, ask.")))?),_=>None,
                };
                let resolved=settings::resolve_permission_config(&manager,&cli,&home,preset).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let preset_bound=resolved.preset==types::PermissionPresetName::Auto;let rules=resolved.rules;
                let approved=storage::load_approved(&ctx.cwd).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let initial_approved=approved.len();let actions=runtime.session_actions()?;let active=actions.get_active_tools()?;
                let disabled=config::disabled(&active,&rules);actions.set_active_tools(active.into_iter().filter(|name|!disabled.contains(name)).collect())?;
                let mut registry = parsers::create_builtin_parser_registry_in(&ctx.agent_dir);
                let rt = runtime.clone();
                registry.set_definition_resolver(Arc::new(move |name: &str| rt.live_tool_definition(name)));
                *state.lock().expect("permission state lock")=Some(SessionPolicy{service:service::PermissionService::new(rules.clone(),approved,events::PermissionEventEmitter{bus}),registry,preset_bound,initial_approved});
                Ok(EventResult::None)
            })
        }));
        let call_state=Arc::clone(&state);let call_counter=Arc::clone(&counter);
        api.on(EventKind::ToolCall,Arc::new(move|event,ctx|{
            let state=Arc::clone(&call_state);let counter=Arc::clone(&call_counter);
            Box::pin(async move{
                let ExtensionEvent::ToolCall(event)=event else{return Ok(EventResult::None)};
                let home=std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let requests={let guard=state.lock().expect("permission state lock");let Some(policy)=guard.as_ref()else{return Ok(EventResult::None)};policy.registry.parse(&event.tool_name,&event.input,(&ctx.cwd,std::path::Path::new(&home)))};
                for parsed in requests{
                    let mut metadata=event.input.as_object().cloned().unwrap_or_default();metadata.insert("toolName".into(),event.tool_name.clone().into());
                    let path=metadata.get("path").or_else(||metadata.get("file_path")).cloned().or_else(||{
                        let patch=metadata.get("input").or_else(||metadata.get("patchText"))?.as_str()?;
                        maho_ext_gpt_apply_patch::text::extract_patched_paths(patch).into_iter().next().map(serde_json::Value::String)
                    });
                    if let Some(path)=path{
                        if config::EDIT_TOOLS.contains(&event.tool_name.as_str()){metadata.insert("filepath".into(),path.clone());}
                        if event.tool_name=="read"{metadata.insert("filePath".into(),path);}
                    }
                    let mut request=types::Request::new(format!("permission-{}",counter.fetch_add(1,std::sync::atomic::Ordering::Relaxed)+1),ctx.session_manager.session_id().into(),parsed.permission,parsed.patterns,parsed.always,metadata);
                    request.original_paths=parsed.original_paths;
                    let ask=service::AskOptions{auto_approve_ask:parsed.auto_approve_ask,rule_aliases:parsed.rule_aliases};
                    let (future,pending,preset_bound)={let mut guard=state.lock().expect("permission state lock");let policy=guard.as_mut().expect("session policy initialized");let future=policy.service.ask_with(request.clone(),ask);let pending=policy.service.list().iter().any(|entry|entry.id==request.id);(future,pending,policy.preset_bound)};
                    if pending{
                        let reply=if ctx.has_ui{prompt::show_permission_prompt(ctx,&request).await}else{
                            // The service already emitted asked; a private bus keeps it from being emitted twice.
                            non_interactive::handle_no_ui(&request,preset_bound,&events::PermissionEventEmitter::default()).map_err(|error|ExtensionFailure::new(error.to_string()))?
                        };
                        if let Some(policy)=state.lock().expect("permission state lock").as_mut(){policy.service.reply(reply);}
                    }
                    if let Err(error)=future.await{return Ok(EventResult::ToolCall(ToolCallEventResult{block:Some(true),reason:Some(error.to_string()),terminate:None}));}
                }
                Ok(EventResult::None)
            })
        }));
        api.on(EventKind::SessionShutdown,Arc::new(move|_,ctx|{
            let state=Arc::clone(&state);
            Box::pin(async move{
                let mut guard=state.lock().expect("permission state lock");if let Some(policy)=guard.as_mut(){
                    let approved=policy.service.get_approved();storage::append_approved(&ctx.cwd,&approved[policy.initial_approved..]).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                    for request in policy.service.list(){policy.service.reply(types::ReplyInput{request_id:request.id,reply:types::Reply::Reject,message:None,always:None});}
                }
                Ok(EventResult::None)
            })
        }));
    }
}

