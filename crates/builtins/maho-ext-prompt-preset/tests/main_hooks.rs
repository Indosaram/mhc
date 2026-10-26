use maho_ext_api::*;
use std::{path::Path, sync::Arc};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}
struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context() -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession),
        model_registry: Arc::new(TestRegistry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
            cwd: "/tmp".into(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}
#[tokio::test]
async fn prompt_main_hooks_match_pinned_reference() {
    let mut ctx = context();
    ctx.cwd = "/nonexistent/todo27-preset".into();
    ctx.agent_dir = ctx.cwd.clone();
    ctx.is_project_trusted_fn = Arc::new(|| false);
    let model: Model = serde_json::from_value(serde_json::json!({"id":"gpt-5.6","name":"gpt-5.6","api":"openai-responses","provider":"custom","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).unwrap();
    ctx.model = Some(model.clone());
    let mut api = ExtensionApi::new(
        LoadedExtension::new("preset", ctx.cwd.clone(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_ext_prompt_preset::PromptPreset.register(&mut api);
    let mut actual = Vec::new();
    let families = [
        "gpt-6-astra",
        "claude-opus-5",
        "kimi-k3",
        "deepseek-v4-1-flash",
        "grok-4.7",
        "glm-5.3",
    ];
    for step in 0..11 {
        let mut options = BuildSystemPromptOptions {
            cwd: "<root>".into(),
            ..Default::default()
        };
        if step == 1 {
            options.append_system_prompt = Some("fixture append".into());
        }
        if step == 2 || step == 3 {
            options.custom_prompt = Some("custom".into());
        }
        let mut selected = model.clone();
        if step == 4 {
            selected.id = "unknown".into();
            selected.name = "unknown".into();
        }
        if step >= 5 {
            selected.id = families[step - 5].into();
            selected.name = families[step - 5].into();
        }
        let mut event = if step == 0 || step == 2 {
            ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
                prompt: String::new(),
                trigger: "prompt".into(),
                preview: false,
                images: None,
                system_prompt: "base".into(),
                system_prompt_options: options,
            })
        } else {
            ExtensionEvent::ModelSelect(ModelSelectEvent {
                model: selected,
                previous_model: None,
                source: ModelSelectSource::Set,
                system_prompt: "base".into(),
                system_prompt_options: options,
            })
        };
        let result = api.registered.handlers[&event.kind()][0](&mut event, &ctx)
            .await
            .unwrap();
        actual.push(match result {
            EventResult::None => serde_json::Value::Null,
            EventResult::BeforeAgentStart(result) => {
                serde_json::json!({"systemPrompt":result.system_prompt})
            }
            EventResult::ModelSelect(result) => {
                let mut value = serde_json::json!({"systemPrompt":result.system_prompt.unwrap()});
                if let Some(name) = result.system_prompt_name {
                    value["systemPromptName"] = name.into();
                }
                value
            }
            _ => panic!("unexpected prompt result"),
        });
    }
    for value in &mut actual {
        if let Some(prompt) = value
            .get_mut("systemPrompt")
            .and_then(|value| value.as_str().map(str::to_owned))
        {
            let start = prompt.find("<workstation>\n").unwrap();
            let end = prompt.find("</workstation>").unwrap() + "</workstation>".len();
            let prompt = format!(
                "{}<workstation>\n- OS: darwin (kernel Darwin 27.0.0)\n- Arch: arm64\n- CPU: Apple M3 Pro (12 cores)\n- GPU: Apple M3 Pro\n</workstation>{}",
                &prompt[..start],
                &prompt[end..]
            );
            let normalized = prompt
                .lines()
                .map(|line| {
                    if line.starts_with("Current date: ") {
                        "Current date: <date>"
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            value["systemPrompt"] = normalized.into();
        }
    }
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("main-hooks.golden.json")).unwrap();
    for (index, (actual, expected)) in actual.iter().zip(expected.as_array().unwrap()).enumerate() {
        if actual != expected {
            let left = serde_json::to_string(actual).unwrap();
            let right = serde_json::to_string(expected).unwrap();
            let offset = left
                .bytes()
                .zip(right.bytes())
                .position(|(left, right)| left != right)
                .unwrap_or(left.len().min(right.len()));
            panic!(
                "hook {index} first differing byte {offset}: native {:?}; reference {:?}",
                left.get(offset.saturating_sub(100)..(offset + 300).min(left.len())),
                right.get(offset.saturating_sub(100)..(offset + 300).min(right.len()))
            );
        }
    }
    assert_eq!(actual.len(), expected.as_array().unwrap().len());
}

#[tokio::test]
async fn preset_prompt_renders_forwarded_tool_snippets_and_guidelines() {
    let mut ctx = context();
    ctx.cwd = "/nonexistent/preset-tools".into();
    ctx.agent_dir = ctx.cwd.clone();
    ctx.is_project_trusted_fn = Arc::new(|| false);
    let model: Model = serde_json::from_value(serde_json::json!({"id":"gpt-5.6","name":"gpt-5.6","api":"openai-responses","provider":"custom","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).unwrap();
    ctx.model = Some(model);
    let mut api = ExtensionApi::new(
        LoadedExtension::new("preset", ctx.cwd.clone(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_ext_prompt_preset::PromptPreset.register(&mut api);
    let mut options = BuildSystemPromptOptions {
        cwd: "<root>".into(),
        tools: vec!["read".into()],
        prompt_guidelines: vec!["Fixture guideline".into()],
        ..Default::default()
    };
    options
        .tool_snippets
        .insert("read".into(), "Fixture read snippet".into());
    let mut event = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: String::new(),
        trigger: "prompt".into(),
        preview: false,
        images: None,
        system_prompt: "base".into(),
        system_prompt_options: options,
    });
    let EventResult::BeforeAgentStart(result) =
        api.registered.handlers[&EventKind::BeforeAgentStart][0](&mut event, &ctx)
            .await
            .unwrap()
    else {
        panic!("expected preset prompt")
    };
    let prompt = result.system_prompt.unwrap();
    assert!(prompt.contains("- read: Fixture read snippet"));
    assert!(prompt.contains("- Fixture guideline"));
}

struct HeaderUi(std::sync::Mutex<Option<Option<ComponentFactory>>>);
impl ExtensionUi for HeaderUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, factory: Option<ComponentFactory>) {
        *self.0.lock().expect("header slot") = Some(factory);
    }
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
#[tokio::test]
async fn session_start_and_model_select_refresh_the_preset_header() {
    let ui = Arc::new(HeaderUi(std::sync::Mutex::new(None)));
    let mut ctx = context();
    ctx.ui = ui.clone();
    ctx.cwd = "/nonexistent/preset-header".into();
    ctx.agent_dir = ctx.cwd.clone();
    ctx.is_project_trusted_fn = Arc::new(|| false);
    let model: Model = serde_json::from_value(serde_json::json!({"id":"gpt-5.6","name":"gpt-5.6","api":"openai-responses","provider":"custom","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).unwrap();
    ctx.model = Some(model.clone());
    let mut api = ExtensionApi::new(
        LoadedExtension::new("preset", ctx.cwd.clone(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_ext_prompt_preset::PromptPreset.register(&mut api);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent {
        reason: SessionReason::Startup,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    api.registered.handlers[&EventKind::SessionStart][0](&mut start, &ctx)
        .await
        .unwrap();
    let factory =
        ui.0.lock()
            .unwrap()
            .take()
            .expect("header set")
            .expect("preset header");
    let lines = factory(&Theme::default()).render(80);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("Optimized system prompt applied: gpt-5.6"));
    let mut unknown = model;
    unknown.id = "unknown".into();
    unknown.name = "unknown".into();
    let mut select = ExtensionEvent::ModelSelect(ModelSelectEvent {
        model: unknown,
        previous_model: None,
        source: ModelSelectSource::Set,
        system_prompt: "base".into(),
        system_prompt_options: BuildSystemPromptOptions::default(),
    });
    api.registered.handlers[&EventKind::ModelSelect][0](&mut select, &ctx)
        .await
        .unwrap();
    assert!(
        ui.0.lock()
            .unwrap()
            .take()
            .expect("header refreshed")
            .is_none()
    );
}

struct PresetRegistry;
impl ModelRegistry for PresetRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn prompt_preset(&self, _: &Model) -> Option<String> {
        Some("glm-5.3".into())
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
#[tokio::test]
async fn model_prompt_preset_override_selects_the_preset() {
    let mut ctx = context();
    ctx.model_registry = Arc::new(PresetRegistry);
    ctx.cwd = "/nonexistent/preset-override".into();
    ctx.agent_dir = ctx.cwd.clone();
    ctx.is_project_trusted_fn = Arc::new(|| false);
    let model: Model = serde_json::from_value(serde_json::json!({"id":"local-llama","name":"local-llama","api":"openai-completions","provider":"custom","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).unwrap();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("preset", ctx.cwd.clone(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_ext_prompt_preset::PromptPreset.register(&mut api);
    let mut select = ExtensionEvent::ModelSelect(ModelSelectEvent {
        model,
        previous_model: None,
        source: ModelSelectSource::Set,
        system_prompt: "base".into(),
        system_prompt_options: BuildSystemPromptOptions::default(),
    });
    let EventResult::ModelSelect(result) =
        api.registered.handlers[&EventKind::ModelSelect][0](&mut select, &ctx)
            .await
            .unwrap()
    else {
        panic!("expected model select result")
    };
    assert_eq!(result.system_prompt_name.as_deref(), Some("glm-5.3"));
}
