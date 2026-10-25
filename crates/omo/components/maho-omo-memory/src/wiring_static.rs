use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
/// The host registration closure the static wiring installs.
pub type RegisterHost = Arc<dyn Fn(&mut maho_ext_api::ExtensionApi) + Send + Sync>;
#[derive(Clone)]
pub struct MemoryStaticOptions {
    pub register_host: Option<RegisterHost>,
    pub skills_trackers: Option<crate::skills_usage_wiring::SkillsUsageTrackers>,
    pub prompt_handler: Option<Arc<crate::prompt::MemoryPromptHandler>>,
    pub prompt: crate::prompt::MemoryPromptInjectionOptions,
    pub nudge: Arc<Mutex<crate::nudge_wiring::MemoryNudgeWiring>>,
    pub resolve_context: crate::prompt::PromptContextResolver,
    pub resolve_tool_context: crate::tools::MemoryContextResolver,
    pub write_notice: Option<crate::tools::WriteNoticeGate>,
    pub tool_exposure: Option<maho_ext_api::ToolExposure>,
    pub resolve_cwd: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub captured_tools: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub warn: crate::guard::GuardWarning,
    pub edit_notice: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub theme: crate::worker::completion_renderers::ResolveEntryTheme,
    pub memory_write: crate::wiring_memory_write::MemoryWriteOptions,
    pub skills_usage: crate::skills_usage_wiring::SkillsUsageOptions,
    pub triggers: crate::trigger_wiring::ReflectionTriggerWiringOptions,
    /// `MemoryWiringSeams.device_name`: names sync principals in the projection pins ("synced from").
    pub device_name: Option<crate::projection_pin::DeviceNameResolver>,
    /// The resolved memory settings; when set, the soul and agreements notice inputs are wired
    /// (pin `createMemoryNoticeInputs`), otherwise the prompt keeps its caller-supplied resolvers.
    pub load_settings: Option<crate::wiring_notice_inputs::NoticeLoadSettings>,
}
pub struct MemoryStaticRegistration {
    pub skills_usage: crate::skills_usage_wiring::SkillsUsageTrackers,
    pub triggers: Arc<crate::trigger_wiring::ReflectionTriggerWiring>,
}
pub fn recover_pending_reflection(
    reflection_dir: &std::path::Path,
    run_id: String,
    reconcile: &dyn Fn() -> Result<(), String>,
) -> crate::commands::types::ReflectionRequestReceipt {
    let recovery = crate::reflection_stale_active_sync::recover_stale_reflection_holder(
        crate::reflection_stale_active_sync::StaleHolderRecoveryInput {
            reflection_dir,
            reconcile,
            seams: None,
            should_reconcile: None,
        },
    );
    let recovered_run_id = if recovery.notice.is_none() {
        match &recovery.verdict {
            crate::reflection_stale_active_sync::ActiveHolderVerdict::Dead { run_id } => {
                Some(run_id.clone())
            }
            _ => None,
        }
    } else {
        None
    };
    crate::commands::types::ReflectionRequestReceipt {
        disposition: crate::commands::types::ReflectionDisposition::Pending,
        run_id,
        recovered_run_id,
        stuck: recovery.notice,
    }
}
pub fn register_memory_static(
    api: &mut maho_ext_api::ExtensionApi,
    options: MemoryStaticOptions,
) -> MemoryStaticRegistration {
    if let Some(register) = options.register_host {
        register(api);
    }
    crate::worker::completion_renderers::register_reflection_completion_renderer(
        api,
        options.theme.clone(),
    );
    crate::worker::health_alert::register_reflection_health_renderer(api, options.theme.clone());
    crate::agreement_notice::register_agreement_notice(api, options.theme);
    crate::nudge_wiring::MemoryNudgeWiring::register(
        options.nudge,
        api,
        options.resolve_context.clone(),
    );
    let on_commit = crate::soul_notice::soul_commit_notice(
        api,
        options.resolve_tool_context.clone(),
        options.edit_notice.clone(),
        options.warn.clone(),
    );
    crate::soul_notice::register_soul_notice(
        api,
        options.resolve_context.clone(),
        options.edit_notice,
    );
    let mut prompt = options.prompt;
    let handler = options.prompt_handler.unwrap_or_else(|| {
        Arc::new(match options.device_name {
            Some(device_name) => crate::prompt::MemoryPromptHandler {
                pins: crate::projection_pin::NativeProjectionPins::new()
                    .with_device_name(device_name),
                ..crate::prompt::MemoryPromptHandler::persistent()
            },
            None => crate::prompt::MemoryPromptHandler::persistent(),
        })
    });
    if let Some(load_settings) = options.load_settings {
        let sender = Arc::new(maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                "memory-notice-inputs",
                api.cwd.clone(),
                Default::default(),
            ),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        let inputs = Arc::new(crate::wiring_notice_inputs::create_memory_notice_inputs(
            crate::wiring_notice_inputs::MemoryNoticeInputsOptions {
                resolve_context: options.resolve_context.clone(),
                load_settings,
                append_entry: Some(Arc::new(move |custom_type, record| {
                    if let Ok(value) = serde_json::to_value(record) {
                        let _ = sender.append_entry(custom_type, Some(value));
                    }
                })),
            },
        ));
        let soul = inputs.clone();
        prompt.resolve_soul_notice = Some(Arc::new(move |repo, session, identity| {
            soul.resolve_soul_notice(repo, session, identity)
                .map(|notice| notice.map(|notice| notice.sha))
        }));
        let agreements = inputs;
        let failed_warn = options.warn.clone();
        handler.set_agreements_notice(Some(crate::prompt::AgreementsNoticeInput{
            resolve:Arc::new(move|repo,session,identity|agreements.resolve_agreements_notice(repo,session,identity)),
            on_failed:Some(Arc::new(move|notice,session,error|failed_warn(&format!("memory notice input failed; this turn has no such notice: {notice} {session} {error}")))),
        }));
    }
    crate::prompt::register_memory_prompt_handler_with_cache(api, prompt, handler);
    crate::tools::register_memory_tool_surface(
        api,
        options.resolve_tool_context,
        crate::tools::MemoryToolsOptions {
            write_notice: options.write_notice,
            exposure: options.tool_exposure,
            on_commit: Some(on_commit),
            ..Default::default()
        },
    );
    crate::guard::register_memory_guard(
        api,
        options.resolve_context.clone(),
        options.resolve_cwd,
        options.captured_tools,
        options.warn,
    );
    crate::skills_scope::register_memory_skills_scope(api, options.resolve_context);
    let skills_usage = match options.skills_trackers {
        Some(trackers) => {
            crate::skills_usage_wiring::register_skills_usage_with_trackers(
                api,
                options.skills_usage,
                trackers.clone(),
            );
            trackers
        }
        None => crate::skills_usage_wiring::register_skills_usage(api, options.skills_usage),
    };
    crate::wiring_memory_write::register_memory_write_listener(api, options.memory_write);
    let triggers = crate::trigger_wiring::create_reflection_trigger_wiring(options.triggers);
    triggers.register(api);
    MemoryStaticRegistration {
        skills_usage,
        triggers,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_static_registration_installs_tools_and_lifecycle_handlers() {
        struct Theme;
        impl crate::worker::entry_renderers::EntryRenderTheme for Theme {
            fn fg(&self, _: &str, text: &str) -> String {
                text.into()
            }
            fn italic(&self, text: &str) -> String {
                text.into()
            }
        }
        let resolve: crate::prompt::PromptContextResolver = Arc::new(|_| None);
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let registration = register_memory_static(
            &mut api,
            MemoryStaticOptions {
                register_host: None,
                skills_trackers: None,
                prompt_handler: None,
                prompt: crate::prompt::MemoryPromptInjectionOptions {
                    resolve_context: resolve.clone(),
                    create_repo: None,
                    search_exposure: None,
                    resolve_nudge_turns: None,
                    resolve_soul_notice: None,
                },
                nudge: Arc::new(Mutex::new(Default::default())),
                resolve_context: resolve,
                resolve_tool_context: Arc::new(|| None),
                write_notice: None,
                tool_exposure: None,
                resolve_cwd: Arc::new(PathBuf::new),
                captured_tools: Arc::new(Vec::new),
                warn: Arc::new(|_| {}),
                edit_notice: Arc::new(|_| true),
                theme: Arc::new(|_| Arc::new(Theme)),
                memory_write: crate::wiring_memory_write::MemoryWriteOptions {
                    resolve_session: Arc::new(|_| None),
                    on_memory_write: Arc::new(|_| Ok(())),
                    refresh_status: Arc::new(|_, _, _| panic!("unbound")),
                    sync_status: None,
                },
                skills_usage: crate::skills_usage_wiring::SkillsUsageOptions {
                    resolve_context: Arc::new(|_| None),
                    resolve_cwd: Arc::new(PathBuf::new),
                    now_ms: Arc::new(|| 0),
                },
                triggers: crate::trigger_wiring::ReflectionTriggerWiringOptions {
                    resolve_session: Arc::new(|_| None),
                    on_launch: Arc::new(|_| Ok(())),
                    warn: Arc::new(|_| {}),
                },
                device_name: None,
                load_settings: None,
            },
        );
        for kind in [
            maho_ext_api::EventKind::BeforeAgentStart,
            maho_ext_api::EventKind::InputDisposition,
            maho_ext_api::EventKind::ToolCall,
            maho_ext_api::EventKind::ToolResult,
            maho_ext_api::EventKind::ResourcesDiscover,
            maho_ext_api::EventKind::AgentSettled,
        ] {
            assert!(api.registered.handlers.contains_key(&kind));
        }
        assert!(
            api.registered
                .entry_renderers
                .contains_key(crate::worker::health_alert::REFLECTION_HEALTH_ENTRY_TYPE)
        );
        assert!(registration.skills_usage.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn native_static_registration_installs_write_notice_gate() {
        struct Theme;
        impl crate::worker::entry_renderers::EntryRenderTheme for Theme {
            fn fg(&self, _: &str, text: &str) -> String {
                text.into()
            }
            fn italic(&self, text: &str) -> String {
                text.into()
            }
        }
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let context = crate::context::MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let gate_enabled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = {
            let gate_enabled = gate_enabled.clone();
            Arc::new(move || gate_enabled.load(std::sync::atomic::Ordering::Relaxed))
        };
        let resolve: crate::prompt::PromptContextResolver =
            Arc::new(move |_| Some(context.clone()));
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let tool_context = resolve("s1");
        register_memory_static(
            &mut api,
            MemoryStaticOptions {
                register_host: None,
                skills_trackers: None,
                prompt_handler: None,
                prompt: crate::prompt::MemoryPromptInjectionOptions {
                    resolve_context: resolve.clone(),
                    create_repo: None,
                    search_exposure: None,
                    resolve_nudge_turns: None,
                    resolve_soul_notice: None,
                },
                nudge: Arc::new(Mutex::new(Default::default())),
                resolve_context: resolve,
                resolve_tool_context: Arc::new(move || tool_context.clone()),
                write_notice: Some(gate),
                tool_exposure: None,
                resolve_cwd: Arc::new(PathBuf::new),
                captured_tools: Arc::new(Vec::new),
                warn: Arc::new(|_| {}),
                edit_notice: Arc::new(|_| true),
                theme: Arc::new(|_| Arc::new(Theme)),
                memory_write: crate::wiring_memory_write::MemoryWriteOptions {
                    resolve_session: Arc::new(|_| None),
                    on_memory_write: Arc::new(|_| Ok(())),
                    refresh_status: Arc::new(|_, _, _| panic!("unbound")),
                    sync_status: None,
                },
                skills_usage: crate::skills_usage_wiring::SkillsUsageOptions {
                    resolve_context: Arc::new(|_| None),
                    resolve_cwd: Arc::new(PathBuf::new),
                    now_ms: Arc::new(|| 0),
                },
                triggers: crate::trigger_wiring::ReflectionTriggerWiringOptions {
                    resolve_session: Arc::new(|_| None),
                    on_launch: Arc::new(|_| Ok(())),
                    warn: Arc::new(|_| {}),
                },
                device_name: None,
                load_settings: None,
            },
        );
        let tool = api
            .registered
            .tools
            .iter()
            .find(|t| t.definition.name == "memory")
            .expect("memory tool registered by static registration");

        // 1. Gate enabled -> details contains writeNotice with serialized identity
        let call_enabled = maho_ext_api::ToolCall {
            id: "c1",
            params: serde_json::json!({"command":"create","reason":"test notice","file_path":"reference/note.md","description":"test","file_text":"content"}),
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        let res1 = (tool.definition.execute)(call_enabled)
            .await
            .expect("create succeeds");
        let details1 = res1.details.expect("details present");
        assert!(
            details1.get("writeNotice").is_some(),
            "writeNotice must be emitted when gate is true"
        );
        assert_eq!(details1["writeNotice"]["identity"], "agent");

        // 2. Gate disabled -> writeNotice is omitted
        gate_enabled.store(false, std::sync::atomic::Ordering::Relaxed);
        let call_disabled = maho_ext_api::ToolCall {
            id: "c2",
            params: serde_json::json!({"command":"create","reason":"test notice 2","file_path":"reference/note2.md","description":"test","file_text":"content"}),
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        let res2 = (tool.definition.execute)(call_disabled)
            .await
            .expect("create succeeds");
        let details2 = res2.details.expect("details present");
        assert!(
            details2.get("writeNotice").is_none(),
            "writeNotice must be omitted when gate is false"
        );
    }

    struct Entries(Mutex<Vec<(String, serde_json::Value)>>);
    impl maho_ext_api::ExtensionActions for Entries {
        fn send_message(
            &self,
            _: maho_ext_api::CustomMessage,
            _: maho_ext_api::SendMessageOptions,
        ) -> Result<(), maho_ext_api::ExtensionFailure> {
            Ok(())
        }
        fn send_user_message(
            &self,
            _: maho_ext_api::UserMessageContent,
            _: maho_ext_api::SendUserMessageOptions,
        ) -> Result<(), maho_ext_api::ExtensionFailure> {
            Ok(())
        }
        fn append_entry(
            &self,
            kind: &str,
            data: Option<serde_json::Value>,
        ) -> Result<(), maho_ext_api::ExtensionFailure> {
            self.0
                .lock()
                .unwrap()
                .push((kind.into(), data.unwrap_or_default()));
            Ok(())
        }
        fn get_all_tools(
            &self,
        ) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn direct_tool_commit_to_soul_path_appends_soul_notice_only_when_enabled() {
        struct Theme;
        impl crate::worker::entry_renderers::EntryRenderTheme for Theme {
            fn fg(&self, _: &str, text: &str) -> String {
                text.into()
            }
            fn italic(&self, text: &str) -> String {
                text.into()
            }
        }
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let context = crate::context::MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let resolve: crate::prompt::PromptContextResolver = Arc::new(|_| None);
        let enabled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let edit_notice = {
            let enabled = enabled.clone();
            Arc::new(move |identity: &str| {
                assert_eq!(identity, "agent");
                enabled.load(std::sync::atomic::Ordering::Relaxed)
            })
        };
        let api_runtime = maho_ext_api::ExtensionRuntime::default();
        let entries = Arc::new(Entries(Mutex::new(vec![])));
        api_runtime.bind(entries.clone());
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            api_runtime,
        );
        register_memory_static(
            &mut api,
            MemoryStaticOptions {
                register_host: None,
                skills_trackers: None,
                prompt_handler: None,
                prompt: crate::prompt::MemoryPromptInjectionOptions {
                    resolve_context: resolve.clone(),
                    create_repo: None,
                    search_exposure: None,
                    resolve_nudge_turns: None,
                    resolve_soul_notice: None,
                },
                nudge: Arc::new(Mutex::new(Default::default())),
                resolve_context: resolve,
                resolve_tool_context: Arc::new(move || Some(context.clone())),
                write_notice: None,
                tool_exposure: None,
                resolve_cwd: Arc::new(PathBuf::new),
                captured_tools: Arc::new(Vec::new),
                warn: Arc::new(|error| panic!("{error}")),
                edit_notice,
                theme: Arc::new(|_| Arc::new(Theme)),
                memory_write: crate::wiring_memory_write::MemoryWriteOptions {
                    resolve_session: Arc::new(|_| None),
                    on_memory_write: Arc::new(|_| Ok(())),
                    refresh_status: Arc::new(|_, _, _| panic!("unbound")),
                    sync_status: None,
                },
                skills_usage: crate::skills_usage_wiring::SkillsUsageOptions {
                    resolve_context: Arc::new(|_| None),
                    resolve_cwd: Arc::new(PathBuf::new),
                    now_ms: Arc::new(|| 0),
                },
                triggers: crate::trigger_wiring::ReflectionTriggerWiringOptions {
                    resolve_session: Arc::new(|_| None),
                    on_launch: Arc::new(|_| Ok(())),
                    warn: Arc::new(|_| {}),
                },
                device_name: None,
                load_settings: None,
            },
        );
        let tool = api
            .registered
            .tools
            .iter()
            .find(|t| t.definition.name == "memory")
            .unwrap();
        let call = |id: &'static str, params: serde_json::Value| maho_ext_api::ToolCall {
            id,
            params,
            signal: Default::default(),
            on_update: None,
            context: None,
        };
        (tool.definition.execute)(call("c1", serde_json::json!({"command":"create","reason":"note","file_path":"reference/note.md","description":"d","file_text":"x"}))).await.unwrap();
        assert!(
            entries.0.lock().unwrap().is_empty(),
            "non-soul commit must not notify"
        );
        (tool.definition.execute)(call("c2", serde_json::json!({"command":"create","reason":"name myself","file_path":"system/identity.md","description":"d","file_text":"name: x"}))).await.unwrap();
        let recorded = entries.0.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, crate::soul_notice::SOUL_UPDATED_ENTRY_TYPE);
        assert_eq!(recorded[0].1["subject"], "name myself");
        assert_eq!(
            recorded[0].1["affectedPaths"],
            serde_json::json!(["system/identity.md"])
        );
        enabled.store(false, std::sync::atomic::Ordering::Relaxed);
        (tool.definition.execute)(call("c3", serde_json::json!({"command":"str_replace","reason":"rename","file_path":"system/identity.md","old_string":"name: x","new_string":"name: y"}))).await.unwrap();
        assert_eq!(
            entries.0.lock().unwrap().len(),
            1,
            "edit_notice=false suppresses the soul notice"
        );
    }
}
