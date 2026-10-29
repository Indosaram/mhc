use crate::{
    adapter::{descriptors::descriptors, renderers::tool_renderers},
    daemon_tool_client::{call_packaged_daemon_tool, current_senpi_request_context},
    post_edit_diagnostics::{
        LspPostEditSessionState, POST_EDIT_DIAGNOSTICS_WIDGET_KEY, should_run_post_edit_diagnostics,
    },
};
use lsp_core::post_edit::DiagnosticsRunnerError;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionFailure,
    ExtensionWidgetOptions, FlagType, FlagValue, ToolContent, ToolError, ToolResult,
    ToolResultEventResult, WidgetPlacement,
};
use std::sync::{Arc, Mutex};
const TOOLS_FLAG: &str = "omo-senpi-lsp-tools-enabled";
const POST_EDIT_FLAG: &str = "omo-senpi-lsp-post-edit-diagnostics-enabled";
#[derive(Default)]
pub struct LspComponent;
pub type DiagnosticsRunner = Arc<
    dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            lsp_core::post_edit::PostEditDiagnosticsOutcome,
                            DiagnosticsRunnerError,
                        >,
                    > + Send,
            >,
        > + Send
        + Sync,
>;
#[derive(Default)]
pub struct LspComponentOptions {
    pub formatter: Option<maho_omo_formatter::FormatterStep>,
    pub run_diagnostics: Option<DiagnosticsRunner>,
}
impl Extension for LspComponent {
    fn register(&self, api: &mut ExtensionApi) {
        self.register_with_options(api, LspComponentOptions::default());
    }
}
impl LspComponent {
    pub fn register_with_options(&self, api: &mut ExtensionApi, options: LspComponentOptions) {
        self.register_with_startup_deferral(api, options, None);
    }
    pub fn register_with_startup_deferral(
        &self,
        api: &mut ExtensionApi,
        options: LspComponentOptions,
        defer_startup_work: Option<maho_ext_api::DeferredMacrotask>,
    ) {
        api.register_flag(
            TOOLS_FLAG,
            FlagType::Boolean {
                default: Some(true),
            },
            Some("Enable omo-senpi LSP tools.".into()),
        );
        api.register_flag(
            POST_EDIT_FLAG,
            FlagType::Boolean {
                default: Some(true),
            },
            Some("Enable omo-senpi post-edit LSP diagnostics.".into()),
        );
        if api.get_flag(TOOLS_FLAG) == Some(FlagValue::Boolean(false)) {
            return;
        }
        let home = crate::daemon_tool_client::resolve_request_home(
            std::env::var("HOME").ok().as_deref(),
            || dirs::home_dir().unwrap_or_default(),
        );
        let notice_cwd = api.cwd.clone();
        let notice_home = home.clone();
        let notices = Box::new(move || {
            for notice in
                crate::adapter::migration_notices::get_config_notices(&notice_cwd, &notice_home)
            {
                maho_ext_api::diagnostic(&format!(
                    "omo-senpi ignored project-local LSP commands; move custom commands to the user config {}: {} ({:?})",
                    notice.user_config_path.display(),
                    notice.config_path.display(),
                    notice.server_ids
                ));
            }
        });
        if let Some(defer) = defer_startup_work {
            defer(notices);
        } else {
            notices();
        }
        for mut tool in descriptors() {
            let name = tool.name.clone();
            let cwd = api.cwd.clone();
            let home = home.clone();
            let extension_home = home.clone();
            tool.execute = Arc::new(move |call| {
                let name = name.clone();
                let cwd = cwd.clone();
                let home = home.clone();
                Box::pin(async move {
                    let context = current_senpi_request_context(&cwd, &home)?;
                    let args = call.params.as_object().cloned().unwrap_or_default();
                    let result =
                        call_packaged_daemon_tool(&name, args, context, call.signal).await?;
                    if result.is_error {
                        return Err(ToolError::Message(result.text().into()));
                    }
                    let content = result
                        .content
                        .into_iter()
                        .map(serde_json::from_value::<ToolContent>)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(ToolResult {
                        content,
                        details: result.details,
                        structured_content: None,
                        is_error: None,
                    })
                })
            });
            let name = tool.name.clone();
            let home = extension_home;
            let renderers =
                tool_renderers(&name).expect("every LSP descriptor has call and result renderers");
            if let Err(error) = api.register_tool_with_extension_context_and_renderers(
                tool,
                Arc::new(move |_, params, _, _, ctx| {
                    let name = name.clone();
                    let home = home.clone();
                    Box::pin(async move {
                        let context = current_senpi_request_context(&ctx.cwd, &home)
                            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                        let args = params.as_object().cloned().unwrap_or_default();
                        let result = call_packaged_daemon_tool(
                            &name,
                            args,
                            context,
                            ctx.signal.clone().unwrap_or_default(),
                        )
                        .await
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                        daemon_agent_result(result)
                    })
                }),
                renderers,
            ) {
                std::panic::panic_any(error);
            }
        }
        let state = Arc::new(Mutex::new(LspPostEditSessionState::default()));
        let diagnostics_enabled = api.get_flag(POST_EDIT_FLAG) != Some(FlagValue::Boolean(false));
        let formatter = Arc::new(std::sync::OnceLock::<maho_omo_formatter::FormatterStep>::new());
        if let Some(step) = options.formatter {
            let _ = formatter.set(step);
        }
        let run_diagnostics = options.run_diagnostics;
        {
            let caches = Arc::clone(&state);
            let home = home.clone();
            api.on(EventKind::ToolResult,Arc::new(move |event,ctx| {let caches=Arc::clone(&caches);let home=home.clone();let formatter=Arc::clone(&formatter);let run_diagnostics=run_diagnostics.clone();Box::pin(async move {
                let ExtensionEvent::ToolResult(event)=event else {return Ok(EventResult::None);};
                let formatter=formatter.get_or_init(|| {
                    let format_cwd=ctx.cwd.clone();let format_home=home.clone();
                    let loaded=maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {cwd:Some(ctx.cwd.to_string_lossy().into_owned()),..Default::default()});
                    let config=loaded.config.get("formatOnMutation").cloned().and_then(|value|serde_json::from_value(value).ok());
                    maho_omo_formatter::create_formatter_step(maho_omo_formatter::FormatterStepOptions {
                        config,
                        markers:Some(Box::new(|cwd|std::fs::read_dir(cwd).map(|entries|entries.filter_map(Result::ok).filter_map(|entry|entry.file_name().into_string().ok()).collect()).unwrap_or_default())),
                        read_marker:Some(Box::new(|cwd,marker|std::fs::read_to_string(cwd.join(marker)).unwrap_or_default())),
                        daemon_format:Some(Box::new(move |path| {
                            let path=path.to_path_buf();let cwd=format_cwd.clone();let home=format_home.clone();
                            Box::pin(async move {
                                let context=current_senpi_request_context(&cwd,&home).map_err(|error|error.to_string())?;
                                let args=serde_json::json!({"filePath":path}).as_object().cloned().unwrap_or_default();
                                let result=call_packaged_daemon_tool("format",args,context,Default::default()).await.map_err(|error|error.to_string())?;
                                Ok(maho_omo_formatter::DaemonFormatResult {content:Some(result.content.into_iter().map(serde_json::from_value::<ToolContent>).collect::<Result<Vec<_>,_>>().map_err(|error|error.to_string())?),details:result.details,is_error:result.is_error})
                            })
                        })),
                        ..Default::default()
                    })
                });
                let formatted=formatter.format_tool_result(event,&ctx.cwd,Some(ctx.session_manager.session_id())).await;
                let has_format_content=formatted.content.is_some();
                // Nothing to append and no diagnostics to run: skip copying the whole tool result.
                if !has_format_content && formatted.error.as_deref().is_none_or(str::is_empty) && (!diagnostics_enabled || !should_run_post_edit_diagnostics(event)) {return Ok(EventResult::None);}
                let mut after_format=event.clone();
                if let Some(content)=formatted.content {after_format.content.extend(content);}
                let formatted_result=||if has_format_content {EventResult::ToolResult(ToolResultEventResult {content:Some(after_format.content.clone()),details:None,is_error:None,usage:None})} else {EventResult::None};
                if formatted.error.as_deref().is_some_and(|error|!error.is_empty()) {return Ok(EventResult::ToolResult(ToolResultEventResult {content:Some(after_format.content),details:None,is_error:Some(true),usage:None}));}
                if !diagnostics_enabled || !should_run_post_edit_diagnostics(&after_format) {return Ok(formatted_result());}
                if let Some(update)=&ctx.update_tool_hook_status {update("(OmO) Checking LSP Diagnostics");}
                let cache=caches.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_create(Some(ctx.session_manager.session_id()));
                let context=current_senpi_request_context(&ctx.cwd,&home).map_err(|e|ExtensionFailure::new(e.to_string()))?;
                let signal=ctx.signal.clone().unwrap_or_default();
                let cwd=ctx.cwd.to_string_lossy().into_owned();
                let agent_dirs=vec![ctx.agent_dir.to_string_lossy().into_owned()];
                let locate_file=|path:&str| lsp_core::post_edit::classify_post_edit_file_location(path,&lsp_core::post_edit::PostEditFileLocationOptions {cwd:&cwd,agent_dirs:&agent_dirs,temp_dirs:None,home_dirs:None});
                let result=crate::post_edit_diagnostics::append_post_edit_diagnostics_with_locator(&after_format,move |path| {let context=context.clone();let signal=signal.clone();let run=run_diagnostics.clone();async move {
                    if let Some(run)=run {return run(path).await;}
                    let args=serde_json::json!({"filePath":path,"severity":"error"}).as_object().cloned().unwrap_or_default();
                    let result=call_packaged_daemon_tool("lsp_diagnostics",args,context,signal).await.map_err(|e|DiagnosticsRunnerError::new(e.to_string()))?;
                    Ok(crate::post_edit_outcome::post_edit_outcome_from_daemon_result(&result))
                }},Some(&cache),Some(&locate_file)).await;
                if let Some(result)=result {ctx.ui.set_widget(POST_EDIT_DIAGNOSTICS_WIDGET_KEY,None,ExtensionWidgetOptions {placement:WidgetPlacement::BelowEditor,actions:None});if let Some(content)=result.content {return Ok(EventResult::ToolResult(ToolResultEventResult {content:Some(content),details:None,is_error:None,usage:None}));}}
                Ok(formatted_result())
            })}));
        }
        if diagnostics_enabled {
            for kind in [EventKind::SessionStart, EventKind::SessionCompact] {
                let caches = Arc::clone(&state);
                api.on(
                    kind,
                    Arc::new(move |_, ctx| {
                        let caches = Arc::clone(&caches);
                        Box::pin(async move {
                            let mut caches = caches
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let id = Some(ctx.session_manager.session_id());
                            if kind == EventKind::SessionStart {
                                caches.on_session_start(id);
                            } else {
                                caches.reset(id);
                            }
                            Ok(EventResult::None)
                        })
                    }),
                );
            }
        }
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                let state = Arc::clone(&state);
                Box::pin(async move {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .delete(Some(ctx.session_manager.session_id()));
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
fn daemon_agent_result(
    result: lsp_daemon::daemon_client::ToolExecutionResult,
) -> Result<maho_ext_api::AgentToolResult, ExtensionFailure> {
    let content = result
        .content
        .into_iter()
        .map(serde_json::from_value::<maho_ext_api::ContentBlock>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
    Ok(maho_ext_api::AgentToolResult {
        content,
        details: result.details.unwrap_or(serde_json::Value::Null),
        is_error: Some(result.is_error),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{
        EventBus, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo,
    };
    use std::path::PathBuf;
    fn api() -> ExtensionApi {
        ExtensionApi::new(
            LoadedExtension::new("lsp", PathBuf::from("/workspace"), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        )
    }
    #[test]
    fn notices_are_scheduled_without_deferring_tool_registration() {
        let mut api = api();
        let pending = Arc::new(Mutex::new(Vec::new()));
        let queued = Arc::clone(&pending);
        LspComponent.register_with_startup_deferral(
            &mut api,
            LspComponentOptions::default(),
            Some(Arc::new(move |work| {
                queued.lock().expect("startup queue").push(work);
            })),
        );
        assert_eq!(api.registered.tools.len(), 6);
        let work = pending
            .lock()
            .expect("startup queue")
            .pop()
            .expect("deferred notices");
        assert!(pending.lock().expect("startup queue").is_empty());
        work();
    }
    #[test]
    fn structured_daemon_errors_keep_content_and_details() {
        let details =
            serde_json::json!({"error":"missing language server","errorKind":"missing_dependency"});
        let daemon = lsp_daemon::daemon_client::ToolExecutionResult {
            content: vec![serde_json::json!({"type":"text","text":"Install the server"})],
            details: Some(details.clone()),
            is_error: true,
        };
        let result = daemon_agent_result(daemon).unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.details, details);
        assert_eq!(
            serde_json::to_value(&result.content).unwrap()[0]["text"],
            "Install the server"
        );
    }
    #[test]
    fn adapter_has_no_vendored_engine_or_project_trust_override() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for module in [
            "client_wrapper",
            "client",
            "connection",
            "directory_diagnostics",
            "errors",
            "infer_extension",
            "inspector",
            "manager_default",
            "manager_lifecycle",
            "manager_types",
            "manager_wait",
            "manager",
            "process",
            "server_installation",
            "server_resolution",
            "transport",
            "workspace_edit",
        ] {
            assert!(!root.join("src").join(format!("{module}.rs")).exists());
        }
        let forbidden = [
            ["vscode", "jsonrpc"].join("-"),
            ["OMO", "SENPI", "TRUST", "PROJECT", "LSP", "COMMANDS"].join("_"),
            ["SENPI", "TRUST", "PROJECT", "LSP", "COMMANDS"].join("_"),
        ];
        let mut directories = vec![root.join("src")];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let source = std::fs::read_to_string(path).unwrap();
                    assert!(forbidden.iter().all(|pattern| !source.contains(pattern)));
                }
            }
        }
    }
    #[test]
    fn descriptor_fields_preserved() {
        let mut a = api();
        LspComponent.register(&mut a);
        let expected = descriptors();
        for (tool, legacy) in a.registered.tools.iter().zip(expected) {
            assert_eq!(tool.definition.parameters, legacy.parameters);
            assert_eq!(tool.definition.label, legacy.label);
            assert_eq!(tool.definition.execution_mode, legacy.execution_mode);
            assert!(!Arc::ptr_eq(&tool.definition.execute, &legacy.execute));
        }
    }
    #[test]
    fn exact_six_tools() {
        let mut a = api();
        LspComponent.register(&mut a);
        assert_eq!(a.registered.tools.len(), 6);
    }
    #[test]
    fn registers_every_tool_with_executor_and_renderers() {
        let mut a = api();
        LspComponent.register(&mut a);
        assert_eq!(a.registered.erased_tool_renderers.len(), 6);
        for name in crate::adapter::descriptors::TOOL_NAMES {
            assert!(a.registered.erased_tool_renderers.contains_key(*name));
            assert!(a.runtime.extension_tool_executor("lsp", name).is_some());
        }
    }
    #[test]
    fn disabled_tools_register_flags_only() {
        let mut a = api();
        a.runtime.set_flag(TOOLS_FLAG, FlagValue::Boolean(false));
        LspComponent.register(&mut a);
        assert!(a.registered.tools.is_empty());
        assert_eq!(a.registered.flags.len(), 2);
    }
    #[test]
    fn missing_server_still_registers() {
        let mut a = api();
        LspComponent.register(&mut a);
        assert_eq!(a.registered.tools.len(), 6);
        for kind in [
            EventKind::SessionStart,
            EventKind::SessionShutdown,
            EventKind::SessionCompact,
            EventKind::ToolResult,
        ] {
            assert!(a.registered.handlers.contains_key(&kind));
        }
    }
}
