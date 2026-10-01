//! Port of senpi extensions/runner.ts dispatch, using native static registration.
use maho_ext_api::*;
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};

pub type ErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;
pub type HookObserver = Arc<dyn Fn(&ToolHookLifecycleEvent) + Send + Sync>;
pub type WarningListener = Arc<dyn Fn(&str) + Send + Sync>;
struct HookRun { event: ToolHookLifecycleEvent, state: Arc<std::sync::Mutex<(bool, String)>> }
struct ContextSessionManager { session: Arc<dyn SessionManager>, actions: Arc<dyn ExtensionContextActions>, runtime: ExtensionRuntime }
impl ToolSessionManager for ContextSessionManager {
    fn session_id(&self) -> &str { self.session.session_id() }
    fn session_file(&self) -> Option<&std::path::Path> { self.session.session_file() }
}
impl SessionManager for ContextSessionManager {
    fn get_entries(&self) -> Vec<SessionEntry> { self.session.get_entries() }
    fn get_branch(&self) -> Vec<SessionEntry> { self.session.get_branch() }
    fn get_leaf_id(&self) -> Option<String> { self.session.get_leaf_id() }
    fn get_session_name(&self) -> Option<String> { self.session.get_session_name() }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { Some(self) }
}
impl ExtensionContextActions for ContextSessionManager {
    fn assert_active(&self) -> Result<(), ExtensionFailure> { self.runtime.assert_active() }
    fn get_model(&self) -> Option<Model> { self.actions.get_model() }
    fn get_service_tier(&self) -> Option<ServiceTier> { self.actions.get_service_tier() }
    fn get_effective_service_tier(&self) -> Option<ServiceTier> { self.actions.get_effective_service_tier() }
    fn get_scoped_models(&self) -> Vec<ScopedModel> { self.actions.get_scoped_models() }
    fn get_agent_dir(&self) -> std::path::PathBuf { self.actions.get_agent_dir() }
    fn is_idle(&self) -> bool { self.actions.is_idle() }
    fn is_project_trusted(&self) -> bool { self.actions.is_project_trusted() }
    fn get_signal(&self) -> Option<AbortSignal> { self.actions.get_signal() }
    fn abort(&self, source: Option<AbortSource>) { self.actions.abort(source); }
    fn has_pending_messages(&self) -> bool { self.actions.has_pending_messages() }
    fn request_reload(&self) -> ExtensionFuture<'_, ()> { self.actions.request_reload() }
    fn is_compacting(&self) -> bool { self.actions.is_compacting() }
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision> { self.actions.check_reload_veto() }
    fn shutdown(&self) { self.actions.shutdown(); }
    fn get_context_usage(&self) -> Option<ContextUsage> { self.actions.get_context_usage() }
    fn get_compaction_settings(&self) -> CompactionSettings { self.actions.get_compaction_settings() }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> { self.actions.get_prompt_cache_safe_wait_seconds() }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 { self.actions.get_prompt_cache_goal_backstop_max_seconds() }
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings { self.actions.get_prompt_cache_keep_alive_settings() }
    fn get_look_at_settings(&self) -> LookAtSettings { self.actions.get_look_at_settings() }
    fn get_ask_user_settings(&self) -> AskUserSettings { self.actions.get_ask_user_settings() }
    fn get_image_settings(&self) -> ImageSettings { self.actions.get_image_settings() }
    fn session_settings(&self) -> &dyn ExtensionSessionSettings { self.actions.session_settings() }
    fn compact(&self, options: CompactOptions) { self.actions.compact(options); }
    fn prepare_provider_request(&self, messages: Vec<AgentMessage>) -> ExtensionFuture<'_, ProviderRequestPreparation> { self.actions.prepare_provider_request(messages) }
    fn begin_compaction(&self, options: BeginCompactionOptions) -> Option<AbortSignal> { self.actions.begin_compaction(options) }
    fn update_compaction(&self, options: UpdateCompactionOptions) { self.actions.update_compaction(options); }
    fn end_compaction(&self, options: EndCompactionOptions) { self.actions.end_compaction(options); }
    fn get_message_revision(&self) -> u64 { self.actions.get_message_revision() }
    fn apply_compaction(&self, result: CompactionResult, options: ApplyCompactionOptions) -> ExtensionFuture<'_, ApplyCompactionResult> { self.actions.apply_compaction(result, options) }
    fn get_system_prompt(&self) -> String { self.actions.get_system_prompt() }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions { self.actions.get_system_prompt_options() }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources { self.actions.get_loaded_hook_sources() }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools> { self.actions.kernel_tools() }
}

pub struct ExtensionRunner {
    pub extensions: Vec<LoadedExtension>, pub runtime: ExtensionRuntime, pub events: EventBus,
    context: ExtensionContext, error_listeners: Vec<ErrorListener>, pub errors: Vec<ExtensionError>,
    pub warnings: Vec<String>, pub shutdown_warn_ms: u64, pub shutdown_timeout_ms: u64,
    hook_observer: Option<HookObserver>, warning_listener: Option<WarningListener>, next_hook_index: u64,
}
impl ExtensionRunner {
    pub fn new(extensions: Vec<LoadedExtension>, runtime: ExtensionRuntime, events: EventBus, context: ExtensionContext) -> Self {
        Self { extensions, runtime, events, context, error_listeners: Vec::new(), errors: Vec::new(), warnings: Vec::new(),
            shutdown_warn_ms: 2000, shutdown_timeout_ms: 10000, hook_observer: None, warning_listener: None, next_hook_index: 0 }
    }
    pub fn from_static(extensions: Vec<Box<dyn Extension>>, context: ExtensionContext) -> Self {
        let runtime = ExtensionRuntime::default();
        let events = EventBus::default();
        let mut loaded = Vec::new();
        for (index, extension) in extensions.into_iter().enumerate() {
            let path = format!("<inline:{index}>");
            let source_info = SourceInfo { path: path.clone(), source: "inline".into(), ..SourceInfo::default() };
            let mut api = ExtensionApi::new(LoadedExtension::new(&path, context.cwd.clone(), source_info), ExtensionSessionProfile::default(), events.clone(), runtime.clone());
            extension.register(&mut api);
            loaded.push(api.registered);
        }
        Self::new(loaded, runtime, events, context)
    }
    pub fn bind_core(&mut self, actions: Arc<dyn ExtensionActions>, context: ExtensionContext) {
        self.runtime.bind(actions); self.context = context;
    }
    pub fn bind_context_actions(&mut self, actions: Arc<dyn ExtensionContextActions>) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.context.session_manager = Arc::new(ContextSessionManager { session: Arc::clone(&self.context.session_manager), actions, runtime: self.runtime.clone() });
        Ok(())
    }
    pub fn bind_providers(&self, actions: Arc<dyn ExtensionProviderActions>) -> Result<(), ExtensionFailure> { self.runtime.bind_providers(actions) }
    pub fn bind_session_actions(&self, actions: Arc<dyn ExtensionSessionActions>) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        for extension in &self.extensions {
            for activator in &extension.lazy_tool_activators { actions.register_lazy_tool_activator(Arc::clone(activator))?; }
            for (name, hint) in &extension.removed_tool_hints { actions.register_removed_tool_hint(name, hint)?; }
        }
        self.runtime.bind_session_actions(actions); Ok(())
    }
    pub fn create_command_context(&self, actions: Arc<dyn ExtensionCommandContextActions>) -> Result<ExtensionCommandContext, ExtensionFailure> {
        Ok(ExtensionCommandContext { context: self.create_context()?, actions })
    }
    pub async fn invoke_command(&self, name: &str, args: &str, context: &ExtensionCommandContext) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let commands = self.get_registered_commands();
        let resolved = commands.iter().find(|command| command.invocation_name == name).ok_or_else(|| ExtensionFailure::new(format!("Unknown extension command: {name}")))?;
        let extension = self.extensions.iter().find(|extension| extension.commands.iter().any(|command| Arc::ptr_eq(&command.handler, &resolved.command.handler))).ok_or_else(|| ExtensionFailure::new("Command owner is unavailable"))?;
        match extension.command_context_handlers.get(&resolved.command.name) {
            Some(handler) => handler(args, context).await,
            None => (resolved.command.handler)(args, &context.context).await,
        }
    }
    pub fn get_shortcuts(&self) -> BTreeMap<String, ExtensionShortcut> {
        let mut shortcuts = BTreeMap::new();
        for extension in &self.extensions { for (key, shortcut) in &extension.shortcuts { shortcuts.insert(key.to_lowercase(), shortcut.clone()); } }
        shortcuts
    }
    pub fn transform_markdown(&self, markdown: &str, context: &MarkdownTransformContext) -> String {
        let mut transformed = markdown.to_owned();
        for extension in &self.extensions { if let Some(transformer) = &extension.markdown_transformer { transformed = transformer(&transformed, context); } }
        transformed
    }
    pub async fn handle_rpc_request(&self, name: &str, data: JsonValue) -> Result<JsonValue, ExtensionFailure> {
        self.runtime.assert_active()?;
        let name = name.trim();
        if name.is_empty() { return Err(ExtensionFailure::new("Extension RPC request name must not be empty")); }
        let mut handlers = self.extensions.iter().filter_map(|extension| extension.rpc_handlers.get(name));
        let handler = handlers.next().ok_or_else(|| ExtensionFailure::new(format!("Unknown extension RPC request: {name}")))?;
        if handlers.next().is_some() { return Err(ExtensionFailure::new(format!("Multiple extension RPC request handlers registered: {name}"))); }
        let result = handler(data).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn create_context(&self) -> Result<ExtensionContext, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut context = self.context.clone();
        if let Some(actions) = context.session_manager.extension_context_actions() {
            actions.assert_active()?;
            context.model = actions.get_model(); context.service_tier = actions.get_service_tier();
            context.effective_service_tier = actions.get_effective_service_tier(); context.scoped_models = actions.get_scoped_models();
            context.agent_dir = actions.get_agent_dir(); context.signal = actions.get_signal();
        }
        context.loaded_extension_paths = self.extensions.iter().map(|e| e.identity.resolved_path.clone()).collect();
        context.registered_mcp_servers = self.get_registered_mcp_servers();
        Ok(context)
    }
    pub fn invalidate(&self, message: &str) { self.runtime.invalidate(message); self.events.clear(); }
    pub fn on_error(&mut self, listener: ErrorListener) { self.error_listeners.push(listener); }
    pub fn set_tool_hook_lifecycle_observer(&mut self, observer: Option<HookObserver>) { self.hook_observer = observer; }
    pub fn set_warning_listener(&mut self, listener: Option<WarningListener>) { self.warning_listener = listener; }
    pub fn emit_error(&mut self, error: ExtensionError) {
        for listener in &self.error_listeners { listener(&error); }
        self.errors.push(error);
    }
    fn report(&mut self, path: &str, kind: EventKind, error: ExtensionFailure) {
        self.emit_error(ExtensionError { extension_path: path.into(), event: kind.as_str().into(), error: error.message, stack: error.stack });
    }
    pub fn has_handlers(&self, kind: EventKind) -> bool { self.extensions.iter().any(|e| e.handlers.get(&kind).is_some_and(|h| !h.is_empty())) }
    fn handlers(&self, kind: EventKind) -> Vec<(String, ExtensionHandler)> {
        self.extensions.iter().flat_map(|e| e.handlers.get(&kind).into_iter().flatten().map(|h| (e.identity.path.clone(), Arc::clone(h)))).collect()
    }
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut tools: Vec<RegisteredTool> = Vec::new();
        for ext in &self.extensions { for tool in &ext.tools {
            if let Some(existing) = tools.iter_mut().find(|t| t.definition.name == tool.definition.name) {
                if existing.source_info.source == "builtin" && tool.source_info.source != "builtin" { *existing = tool.clone(); }
            } else { tools.push(tool.clone()); }
        }}
        tools
    }
    pub fn get_all_tools(&self) -> Vec<ToolInfo> { self.get_all_registered_tools().into_iter().map(|t| normalize_tool_exposure(&t.definition, t.source_info)).collect() }
    pub fn get_tool_definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.extensions.iter().flat_map(|e| &e.tools).find(|t| t.definition.name == name).map(|t| &t.definition)
    }
    pub fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        let commands: Vec<_> = self.extensions.iter().flat_map(|e| &e.commands).collect();
        let mut counts = BTreeMap::new();
        for command in &commands { *counts.entry(&command.name).or_insert(0usize) += 1; }
        let mut seen = BTreeMap::new(); let mut taken = BTreeSet::new();
        commands.into_iter().map(|command| {
            let occurrence = seen.entry(&command.name).or_insert(0usize); *occurrence += 1;
            let mut suffix = *occurrence;
            let mut name = if counts[&command.name] > 1 { format!("{}:{suffix}", command.name) } else { command.name.clone() };
            while taken.contains(&name) { suffix += 1; name = format!("{}:{suffix}", command.name); }
            taken.insert(name.clone()); ResolvedCommand { command: command.clone(), invocation_name: name }
        }).collect()
    }
    pub fn get_command(&self, name: &str) -> Option<ResolvedCommand> { self.get_registered_commands().into_iter().find(|c| c.invocation_name == name) }
    pub fn get_flags(&self) -> BTreeMap<String, ExtensionFlag> {
        let mut flags = BTreeMap::new(); for ext in &self.extensions { for flag in &ext.flags { flags.entry(flag.name.clone()).or_insert_with(|| flag.clone()); }} flags
    }
    pub fn get_registered_mcp_servers(&self) -> Vec<RegisteredMcpServerDeclaration> {
        let mut servers: Vec<RegisteredMcpServerDeclaration> = Vec::new();
        for ext in &self.extensions { for server in &ext.mcp_servers { if !servers.iter().any(|s| s.name == server.name) { servers.push(server.clone()); } }} servers
    }
    pub fn get_mcp_server_diagnostics(&self) -> Vec<String> {
        let mut owners: BTreeMap<&str, &str> = BTreeMap::new(); let mut warnings = Vec::new();
        for ext in &self.extensions { for server in &ext.mcp_servers {
            if let Some(owner) = owners.get(server.name.as_str()) {
                warnings.push(format!("MCP server '{}' declared by both {} and {}; keeping first declaration from {}.", server.name, owner, ext.identity.path, owner));
            } else { owners.insert(&server.name, &ext.identity.path); }
        }} warnings
    }
    pub fn get_message_renderer(&self, custom_type: &str) -> Option<&MessageRenderer> { self.extensions.iter().find_map(|e| e.message_renderers.get(custom_type)) }
    pub fn get_entry_renderer(&self, custom_type: &str) -> Option<&EntryRenderer> { self.extensions.iter().find_map(|e| e.entry_renderers.get(custom_type)) }
    pub fn get_entry_renderer_options(&self, custom_type: &str) -> Option<&EntryRendererOptions> { self.extensions.iter().find(|e| e.entry_renderers.contains_key(custom_type)).and_then(|e| e.entry_renderer_options.get(custom_type)) }
    pub fn get_filesystem_policy_denied_roots(&self) -> Vec<std::path::PathBuf> { self.extensions.iter().flat_map(|e| &e.filesystem_policies).flat_map(|p| p.denied_roots.clone().unwrap_or_default()).collect() }

    pub async fn emit(&mut self, mut event: ExtensionEvent) -> Result<EventResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let kind = event.kind(); let mut result = EventResult::None;
        for (path, handler) in self.handlers(kind) {
            let context = self.create_context()?;
            let handled = if kind == EventKind::SessionShutdown { self.run_shutdown(&path, &mut event, &context, &handler).await } else { handler(&mut event, &context).await };
            match handled {
                Ok(next) => { if kind.is_session_before() && let EventResult::SessionBefore(before) = &next { let cancel = before.cancel == Some(true); result = next; if cancel { return Ok(result); } } }
                Err(error) => self.report(&path, kind, error),
            }
        }
        Ok(result)
    }
    async fn run_shutdown(&mut self, path: &str, event: &mut ExtensionEvent, context: &ExtensionContext, handler: &ExtensionHandler) -> Result<EventResult, ExtensionFailure> {
        let signal = AbortSignal::default();
        if let ExtensionEvent::SessionShutdown(shutdown) = event { shutdown.signal = Some(signal.clone()); }
        let future = handler(event, context); tokio::pin!(future);
        let warn = tokio::time::sleep(Duration::from_millis(self.shutdown_warn_ms)); tokio::pin!(warn);
        let cap = tokio::time::sleep(Duration::from_millis(self.shutdown_timeout_ms)); tokio::pin!(cap);
        let mut warned = false;
        loop {
            tokio::select! {
                result = &mut future => return result,
                () = &mut warn, if self.shutdown_warn_ms > 0 && !warned => {
                    warned = true;
                    let message = format!("Extension {path} is still running its session_shutdown handler after {}ms.", self.shutdown_warn_ms);
                    if let Some(listener) = &self.warning_listener { listener(&message); }
                    self.warnings.push(message);
                }
                () = &mut cap, if self.shutdown_timeout_ms > 0 => {
                    signal.abort(); return Err(ExtensionFailure::new(format!("handler timed out after {}ms", self.shutdown_timeout_ms)));
                }
            }
        }
    }
    pub async fn emit_before_agent_start(&mut self, event: BeforeAgentStartEvent) -> Result<Option<BeforeAgentStartCombinedResult>, ExtensionFailure> {
        let mut event = ExtensionEvent::BeforeAgentStart(event); let mut combined = BeforeAgentStartCombinedResult::default();
        for (path, handler) in self.handlers(EventKind::BeforeAgentStart) {
            let mut context = self.create_context()?;
            if let ExtensionEvent::BeforeAgentStart(current) = &event { let prompt = current.system_prompt.clone(); context.get_system_prompt_fn = Arc::new(move || prompt.clone()); }
            match handler(&mut event, &context).await {
                Ok(EventResult::BeforeAgentStart(next)) => {
                    if let Some(message) = next.message { combined.messages.push(message); }
                    if let Some(prompt) = next.system_prompt { if let ExtensionEvent::BeforeAgentStart(current) = &mut event { current.system_prompt = prompt.clone(); } combined.system_prompt = Some(prompt); }
                }
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::BeforeAgentStart, error),
            }
        }
        Ok(if combined.messages.is_empty() && combined.system_prompt.is_none() { None } else { Some(combined) })
    }
    pub async fn emit_input(&mut self, input: InputEvent) -> Result<InputEventResult, ExtensionFailure> {
        let original_text = input.text.clone(); let original_images = input.images.clone();
        let mut event = ExtensionEvent::Input(input);
        for (path, handler) in self.handlers(EventKind::Input) {
            let context = self.create_context()?;
            match handler(&mut event, &context).await {
                Ok(EventResult::Input(InputEventResult::Handled)) => return Ok(InputEventResult::Handled),
                Ok(EventResult::Input(InputEventResult::Transform { text, images })) => if let ExtensionEvent::Input(current) = &mut event { current.text = text; if images.is_some() { current.images = images; } },
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::Input, error),
            }
        }
        if let ExtensionEvent::Input(current) = event { return Ok(if current.text != original_text || current.images != original_images { InputEventResult::Transform { text: current.text, images: current.images } } else { InputEventResult::Continue }); }
        Err(ExtensionFailure::new("input handler replaced event kind"))
    }
    pub async fn emit_model_select(&mut self, event: ModelSelectEvent) -> Result<Option<ModelSelectEventResult>, ExtensionFailure> {
        let mut event = ExtensionEvent::ModelSelect(event); let mut combined: Option<ModelSelectEventResult> = None;
        for (path, handler) in self.handlers(EventKind::ModelSelect) {
            let context = self.create_context()?;
            if let ExtensionEvent::ModelSelect(current) = &mut event { current.system_prompt_options = context.get_system_prompt_options(); }
            match handler(&mut event, &context).await {
                Ok(EventResult::ModelSelect(next)) if next.system_prompt.is_some() || next.system_prompt_name.is_some() => {
                    let result = combined.get_or_insert_with(Default::default);
                    if next.system_prompt.is_some() { result.system_prompt = next.system_prompt; }
                    if next.system_prompt_name.is_some() { result.system_prompt_name = next.system_prompt_name; }
                }
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::ModelSelect, error),
            }
        }
        Ok(combined)
    }
    pub async fn emit_message_end(&mut self, message: AgentMessage) -> Result<Option<AgentMessage>, ExtensionFailure> {
        let mut event = ExtensionEvent::MessageEnd { message }; let mut modified = false;
        for (path, handler) in self.handlers(EventKind::MessageEnd) {
            let context = self.create_context()?;
            match handler(&mut event, &context).await {
                Ok(EventResult::MessageEnd { message: Some(next) }) => if let ExtensionEvent::MessageEnd { message } = &mut event {
                    if next.role() != message.role() { self.report(&path, EventKind::MessageEnd, ExtensionFailure::new("message_end handlers must return a message with the same role")); }
                    else { *message = next; modified = true; }
                },
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::MessageEnd, error),
            }
        }
        if let ExtensionEvent::MessageEnd { message } = event { return Ok(modified.then_some(message)); }
        Err(ExtensionFailure::new("message_end handler replaced event kind"))
    }
    pub async fn emit_context(&mut self, messages: &[AgentMessage], exclude_path: Option<&str>) -> Result<Vec<AgentMessage>, ExtensionFailure> {
        let mut event = ExtensionEvent::Context { messages: messages.to_vec() };
        for (path, handler) in self.handlers(EventKind::Context) {
            if exclude_path == Some(path.as_str()) { continue; }
            let context = self.create_context()?;
            match handler(&mut event, &context).await {
                Ok(EventResult::Context { messages: Some(next) }) => if let ExtensionEvent::Context { messages } = &mut event { *messages = next; },
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::Context, error),
            }
        }
        if let ExtensionEvent::Context { messages } = event { return Ok(messages); }
        Err(ExtensionFailure::new("context handler replaced event kind"))
    }
    pub async fn emit_before_provider_request(&mut self, payload: JsonValue, exclude_path: Option<&str>) -> Result<JsonValue, ExtensionFailure> {
        let mut event = ExtensionEvent::BeforeProviderRequest { payload, model: None, headers: None };
        for (path, handler) in self.handlers(EventKind::BeforeProviderRequest) {
            if exclude_path == Some(path.as_str()) { continue; }
            let context = self.create_context()?;
            match handler(&mut event, &context).await {
                Ok(EventResult::ProviderPayload(next)) => if let ExtensionEvent::BeforeProviderRequest { payload, .. } = &mut event { *payload = next; },
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::BeforeProviderRequest, error),
            }
        }
        if let ExtensionEvent::BeforeProviderRequest { payload, .. } = event { return Ok(payload); }
        Err(ExtensionFailure::new("provider handler replaced event kind"))
    }
    pub async fn emit_before_provider_headers(&mut self, headers: BTreeMap<String, Option<String>>) -> Result<BTreeMap<String, Option<String>>, ExtensionFailure> {
        let mut event = ExtensionEvent::BeforeProviderHeaders { headers };
        for (path, handler) in self.handlers(EventKind::BeforeProviderHeaders) {
            let context = self.create_context()?;
            if let Err(error) = handler(&mut event, &context).await { self.report(&path, EventKind::BeforeProviderHeaders, error); }
        }
        if let ExtensionEvent::BeforeProviderHeaders { headers } = event { return Ok(headers); }
        Err(ExtensionFailure::new("header handler replaced event kind"))
    }
    pub async fn emit_project_trust(&mut self, cwd: std::path::PathBuf) -> Result<Option<ProjectTrustEventResult>, ExtensionFailure> {
        let mut event = ExtensionEvent::ProjectTrust { cwd };
        for (path, handler) in self.handlers(EventKind::ProjectTrust) {
            let context = self.create_context()?;
            match handler(&mut event, &context).await {
                Ok(EventResult::ProjectTrust(result)) if result.trusted != TrustDecision::Undecided => return Ok(Some(result)),
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::ProjectTrust, error),
            }
        }
        Ok(None)
    }
    pub async fn emit_resources_discover(&mut self, cwd: std::path::PathBuf, reason: SessionReason) -> Result<DiscoveredResources, ExtensionFailure> {
        let mut event = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent { cwd, reason, scoped_entries: true });
        let mut combined = DiscoveredResources::default();
        for (path, handler) in self.handlers(EventKind::ResourcesDiscover) {
            let context = self.create_context()?;
            match handler(&mut event, &context).await {
                Ok(EventResult::ResourcesDiscover(next)) => {
                    let convert = |entries: Vec<ResourceDiscoverEntry>| entries.into_iter().map(|e| DiscoveredResourceEntry { path: e.path, scope: e.scope, extension_path: path.clone() }).collect::<Vec<_>>();
                    combined.skill_paths.extend(convert(next.skill_paths)); combined.prompt_paths.extend(convert(next.prompt_paths));
                    combined.theme_paths.extend(convert(next.theme_paths)); combined.hook_paths.extend(convert(next.hook_paths));
                }
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::ResourcesDiscover, error),
            }
        }
        Ok(combined)
    }
    pub async fn emit_user_bash(&mut self, command: String, exclude_from_context: bool, cwd: std::path::PathBuf) -> Result<EventResult, ExtensionFailure> {
        let mut event = ExtensionEvent::UserBash { command, exclude_from_context, cwd };
        for (path, handler) in self.handlers(EventKind::UserBash) {
            let context = self.create_context()?;
            match handler(&mut event, &context).await { Ok(EventResult::None) => {}, Ok(result) => return Ok(result), Err(error) => self.report(&path, EventKind::UserBash, error) }
        }
        Ok(EventResult::None)
    }
    fn begin_hook(&mut self, path: &str, tool_name: &str, tool_call_id: &str, hook_name: ToolHookName, context: &mut ExtensionContext) -> HookRun {
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let name = match hook_name { ToolHookName::PreToolUse => "PreToolUse", ToolHookName::PostToolUse => "PostToolUse" };
        let base = ToolHookLifecycleEvent { hook_run_id: format!("{tool_call_id}:{name}:{}", self.next_hook_index), hook_name, tool_name: tool_name.into(), tool_call_id: tool_call_id.into(), extension_path: path.into(), status_message: hook_status_message(path, hook_name), started_at, phase: ToolHookPhase::Start };
        self.next_hook_index = self.next_hook_index.wrapping_add(1);
        if let Some(observer) = &self.hook_observer { observer(&base); }
        let updates = Arc::new(std::sync::Mutex::new((false, base.status_message.clone())));
        let state = Arc::clone(&updates); let observer = self.hook_observer.clone(); let mut update_event = base.clone(); update_event.phase = ToolHookPhase::Update;
        context.update_tool_hook_status = Some(Arc::new(move |message| {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner); if state.0 { return; }
            state.1 = sanitize_status(message); let mut event = update_event.clone(); event.status_message = state.1.clone();
            if let Some(observer) = &observer { observer(&event); }
        }));
        HookRun { event: base, state: updates }
    }
    fn end_hook(&self, hook: HookRun, status: ToolHookStatus, error_message: Option<String>) {
        let mut event = hook.event;
        let mut state = hook.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.0 = true; event.status_message = state.1.clone(); drop(state);
        event.phase = ToolHookPhase::End { completed_at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)), status, error_message };
        if let Some(observer) = &self.hook_observer { observer(&event); }
    }
    pub async fn emit_tool_call(&mut self, event: &mut ToolCallEvent) -> Result<Option<ToolCallEventResult>, ExtensionFailure> {
        let mut current = ExtensionEvent::ToolCall(event.clone()); let mut combined = None;
        for (path, handler) in self.handlers(EventKind::ToolCall) {
            let mut context = self.create_context()?; let hook = self.begin_hook(&path, &event.tool_name, &event.tool_call_id, ToolHookName::PreToolUse, &mut context);
            let result = handler(&mut current, &context).await;
            let (status, error) = match &result { Err(e) => (ToolHookStatus::Failed, Some(e.message.clone())), Ok(EventResult::ToolCall(r)) if r.block == Some(true) => (ToolHookStatus::Blocked, None), Ok(_) => (ToolHookStatus::Completed, None) };
            self.end_hook(hook, status, error);
            if let ExtensionEvent::ToolCall(updated) = &current { *event = updated.clone(); }
            if let EventResult::ToolCall(next) = result? { let blocked = next.block == Some(true); combined = Some(next); if blocked { return Ok(combined); } }
        }
        Ok(combined)
    }
    pub async fn emit_tool_result(&mut self, event: ToolResultEvent) -> Result<Option<ToolResultEventResult>, ExtensionFailure> {
        let tool_name = event.tool_name.clone(); let tool_call_id = event.tool_call_id.clone();
        let mut current = ExtensionEvent::ToolResult(event); let mut modified = false;
        for (path, handler) in self.handlers(EventKind::ToolResult) {
            let mut context = self.create_context()?; let hook = self.begin_hook(&path, &tool_name, &tool_call_id, ToolHookName::PostToolUse, &mut context);
            let result = handler(&mut current, &context).await;
            self.end_hook(hook, if result.is_err() { ToolHookStatus::Failed } else { ToolHookStatus::Completed }, result.as_ref().err().map(|e| e.message.clone()));
            match result {
                Ok(EventResult::ToolResult(next)) => if let ExtensionEvent::ToolResult(event) = &mut current {
                    if let Some(content) = next.content { event.content = content; modified = true; }
                    if let Some(details) = next.details { event.details = Some(details); modified = true; }
                    if let Some(is_error) = next.is_error { event.is_error = is_error; modified = true; }
                    if let Some(usage) = next.usage { event.usage = Some(usage); modified = true; }
                },
                Ok(_) => {}, Err(error) => self.report(&path, EventKind::ToolResult, error),
            }
        }
        if let ExtensionEvent::ToolResult(event) = current { return Ok(modified.then_some(ToolResultEventResult { content: Some(event.content), details: event.details, is_error: Some(event.is_error), usage: event.usage })); }
        Err(ExtensionFailure::new("tool_result handler replaced event kind"))
    }
}

pub fn format_extension_error_headline(error: &ExtensionError) -> String {
    let path = sanitize_terminal(&error.extension_path); let event = sanitize_terminal(&error.event); let body = sanitize_terminal(&error.error);
    if path == RUNTIME_EXTENSION_PATH { if event.is_empty() { format!("Runtime error: {body}") } else { format!("Runtime error ({event}): {body}") } }
    else { format!("Extension \"{path}\" error: {body}") }
}
fn sanitize_terminal(text: &str) -> String {
    let mut output = String::new(); let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' { match chars.next() {
            Some('[') => { for c in chars.by_ref() { if ('@'..='~').contains(&c) { break; } } },
            Some(']' | 'P' | '_' | '^' | 'X') => { while let Some(c) = chars.next() { if c == '\u{7}' || c == '\u{9c}' { break; }
                if c == '\u{1b}' && chars.peek() == Some(&'\\') { chars.next(); break; } } },
            Some(_) | None => {},
        }} else if ch == '\u{9b}' { for c in chars.by_ref() { if ('@'..='~').contains(&c) { break; } } }
        else if ch == '\r' { output.push('\n'); if chars.peek() == Some(&'\n') { chars.next(); } }
        else if ch == ' ' || ch == '\t' { if !output.ends_with(' ') { output.push(' '); } }
        else if !ch.is_control() || ch == '\n' { output.push(ch); }
    }
    output
}
fn bounded_status(message: &str) -> String { if message.chars().count() > 79 { format!("{}...", message.chars().take(76).collect::<String>()) } else { message.into() } }
fn sanitize_status(message: &str) -> String { bounded_status(&sanitize_terminal(message).split_whitespace().collect::<Vec<_>>().join(" ")) }
fn hook_status_message(path: &str, hook_name: ToolHookName) -> String {
    match path {
        "<builtin:permission-system>" => "matching project rules".into(), "<builtin:bash-timeout>" => "applying bash timeout".into(),
        "<builtin:compaction>" => match hook_name { ToolHookName::PreToolUse => "checking compaction state".into(), ToolHookName::PostToolUse => "checking tool result size".into() },
        "<builtin:tool-pair-guard>" => "checking tool/result pairs".into(),
        _ => { let name = path.strip_prefix("<builtin:").and_then(|s| s.strip_suffix('>')).or_else(|| path.strip_prefix('<').and_then(|s| s.strip_suffix('>'))).unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path));
            let name = name.rsplit_once('.').map_or(name, |(stem, _)| stem); bounded_status(&format!("running {name}")) }
    }
}
