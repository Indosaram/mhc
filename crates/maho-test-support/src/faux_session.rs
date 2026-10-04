use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use serde_json::Value;

use crate::faux::FauxScript;

mod context;

struct SharedExtension(std::sync::Arc<maho_ext_host::loader::NativeExtensionFactory>);

impl maho_ext_api::Extension for SharedExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        self.0.extension.register(api);
    }
}

pub struct FauxSession {
    pub scenario: String,
    pub omo: bool,
    pub script: FauxScript,
    native_extensions: Vec<std::sync::Arc<maho_ext_host::loader::NativeExtensionFactory>>,
    native_responses: Option<Vec<maho_ai::types::AssistantMessage>>,
}

impl FauxSession {
    pub fn new(script: FauxScript) -> Self {
        Self {
            scenario: script.name.clone(),
            omo: false,
            script,
            native_extensions: Vec::new(),
            native_responses: None,
        }
    }

    pub fn with_extension(mut self, _ext: impl Into<String>) -> Self {
        self.omo = true;
        self
    }

    /// Register a native extension for each in-process run, before session startup.
    pub fn with_native_extension(mut self, factory: maho_ext_host::loader::NativeExtensionFactory) -> Self {
        self.native_extensions.push(std::sync::Arc::new(factory));
        self
    }

    /// Supply typed faux responses, including tool calls, instead of the text script responses.
    pub fn with_native_responses(mut self, responses: Vec<maho_ai::types::AssistantMessage>) -> Self {
        self.native_responses = Some(responses);
        self
    }

    pub fn run_and_serialize(&self) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        run_faux_scenario(&self.scenario, self.omo)
    }

    /// Boots the session and returns a handle instead of a serialized event log.
    ///
    /// The handle owns a live `AgentSession` over the faux provider with the registered native
    /// extensions bound, so a test can drive prompts, tool execution and provider payloads offline
    /// (fixed clock/seed, temp HOME; no network, provider or auth call).
    pub async fn run_native_handle(&self) -> Result<NativeSession, Box<dyn std::error::Error + Send + Sync>> {
        self.boot_native().await
    }

    pub async fn run_native(&self) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        use std::sync::Mutex;
        use maho_core::agent_session::PromptOptions;

        let NativeSession { session, runner: _runner, provider: _provider, script: _script, calls: _calls, temp: _temp } =
            self.boot_native().await?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            let value = match event {
                maho_ext_api::AgentSessionEvent::Agent(event) => serde_json::to_value(event).ok(),
                _ => None,
            };
            if let Some(value) = value {
                captured.lock().unwrap_or_else(PoisonError::into_inner).push(value);
            }
        }));
        session.prompt(&self.script.prompt, PromptOptions::default()).await?;
        // Flat projection (senpi's message shape): the agent's messages are serialized as the
        // flat user/assistant/custom objects, not the `Llm`/`Custom` enum wrapper that
        // `serde_json::to_value(Vec<AgentMessage>)` produces.
        let messages = serde_json::Value::Array(session.messages().iter()
            .map(maho_core::agent_session::session_message_to_value)
            .collect::<Result<Vec<_>, _>>()?);
        let entries = session.with_session_manager(|manager| manager.entries());
        let events = events.lock().unwrap_or_else(PoisonError::into_inner).clone();
        // Canonical teardown, mirroring AgentSessionRuntime::dispose: a bare AgentSession::dispose
        // does not emit session_shutdown, so an extension that suspends on it (e.g. the loop
        // extension) would be left in its pre-shutdown phase.
        session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
        session.dispose().await;
        Ok(serde_json::json!({ "scenario": self.scenario, "events": events, "entries": entries, "messages": messages }))
    }

    fn scripted_messages(&self) -> Result<Vec<maho_ai::types::AssistantMessage>, Box<dyn std::error::Error + Send + Sync>> {
        use maho_ai::providers::faux::{FauxAssistantMessageOptions, faux_assistant_message};

        if let Some(responses) = &self.native_responses {
            return Ok(responses.clone());
        }
        self.script.responses.iter().map(|response| {
            let stop_reason = serde_json::from_value::<maho_ai::types::StopReason>(Value::String(response.stop_reason.clone()))?;
            Ok(faux_assistant_message(response.content.clone(), FauxAssistantMessageOptions {
                stop_reason: Some(stop_reason), timestamp: Some(0), ..Default::default()
            }))
        }).collect::<Result<Vec<_>, serde_json::Error>>().map_err(Into::into)
    }

    fn load_native_extensions(&self, cwd: &Path) -> maho_ext_host::loader::LoadExtensionsResult {
        maho_ext_host::loader::load_extensions(self.native_extensions.iter().map(|factory| {
            maho_ext_host::loader::NativeExtensionFactory {
                path: factory.path.clone(), source_info: factory.source_info.clone(),
                extension: Box::new(SharedExtension(Arc::clone(factory))),
            }
        }).collect(), cwd, maho_ext_api::ExtensionSessionProfile::default())
    }

    fn new_agent_session(&self, provider: &maho_ai::providers::faux::FauxProviderHandle, model: maho_ai::types::Model,
        cwd: &str, temp: &Path) -> Result<maho_core::agent_session::AgentSession, Box<dyn std::error::Error + Send + Sync>> {
        use maho_core::agent_session::{AgentSession, AgentSessionConfig};

        let streams = maho_ai::providers::faux::faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        });
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(temp.join("models.json")),
            auth_path: Some(temp.join("auth.json")),
            providers: Some(vec![provider.provider.clone()]), ..Default::default()
        });
        Ok(AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
                stream_fn: Some(stream_fn), ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
            cwd: cwd.to_owned(), agent_dir: Some(cwd.to_owned()), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
            scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
            model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
            initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None,
            allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
            session_start_event: None, auto_title_sessions: Some(false),
        })?)
    }

    async fn boot_native(&self) -> Result<NativeSession, Box<dyn std::error::Error + Send + Sync>> {
        use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};

        if self.omo && self.native_extensions.is_empty() {
            return Err("Native faux sessions require native extension registration; the bun golden runner remains available".into());
        }
        let temp = tempfile::tempdir()?;
        let cwd = temp.path().to_string_lossy().into_owned();
        let provider = faux_provider(RegisterFauxProviderOptions {
            api: Some("faux".to_owned()), tokens_per_second: Some(0.0), ..Default::default()
        });
        let model = provider.get_model(Some("faux-1")).ok_or("Missing faux-1 model")?;
        let script = self.scripted_messages()?;
        let calls = Arc::new(Mutex::new(Vec::new()));
        provider.set_responses(script.iter().map(|message| recording_step(message.clone(), calls.clone(), None)).collect());
        let session = self.new_agent_session(&provider, model, &cwd, temp.path())?;
        let extensions = self.load_native_extensions(temp.path());
        let runner = if extensions.extensions.is_empty() {
            maho_ext_host::ExtensionRunner::new(Vec::new(), Default::default(), Default::default(), context::create(&session))
        } else {
            bind_native_extensions(&session, extensions).await
        };
        Ok(NativeSession { session, runner, provider, script, calls, temp })
    }
}

async fn bind_native_extensions(session: &maho_core::agent_session::AgentSession,
    extensions: maho_ext_host::loader::LoadExtensionsResult) -> maho_ext_host::ExtensionRunner {
    let runner = maho_ext_host::ExtensionRunner::new(
        extensions.extensions, extensions.runtime, extensions.events, context::create(session),
    );
    let registered = runner.get_all_registered_tools();
    let runtime = runner.runtime.clone();
    let handle = runner.clone();
    session.set_extension_runner(runner).await;
    let mut names = session.get_active_tool_names();
    for registered in registered {
        names.push(registered.definition.name.clone());
        let tool_session = session.weak_accessor();
        let tool = maho_ext_host::wrapper::wrap_registered_tool(registered.clone(), runtime.clone(),
            Arc::new(move || tool_session().map(|session| context::create(&session))
                .ok_or_else(|| maho_ext_api::ExtensionFailure::new("Session disposed"))));
        session.register_tool_definition(registered.definition, registered.source_info, tool);
    }
    session.set_active_tools_by_name(names);
    session.bind_extensions(maho_core::agent_session::ExtensionBindings {
        mode: Some(maho_ext_api::ExtensionMode::Print), ..Default::default()
    }).await;
    handle
}

/// One provider call the session made, in order.
#[derive(Clone)]
pub struct ProviderCall {
    pub context: maho_ai::types::Context,
    pub options: Option<maho_ai::types::SimpleStreamOptions>,
}

/// A scripted gate that holds the next provider response until [`ResponseGate::release`].
#[derive(Clone)]
pub struct ResponseGate {
    inner: Arc<ResponseGateInner>,
}

struct ResponseGateInner {
    entered: tokio::sync::watch::Sender<bool>,
    released: tokio::sync::watch::Sender<bool>,
    // Keep one receiver alive so `send` never fails with "no receivers" before a waiter subscribes;
    // `entered()`/`release()` still subscribe a fresh receiver that observes the current value.
    _entered_keep: tokio::sync::watch::Receiver<bool>,
    _released_keep: tokio::sync::watch::Receiver<bool>,
}

impl ResponseGate {
    fn new() -> Self {
        let (entered, entered_keep) = tokio::sync::watch::channel(false);
        let (released, released_keep) = tokio::sync::watch::channel(false);
        Self { inner: Arc::new(ResponseGateInner { entered, released, _entered_keep: entered_keep, _released_keep: released_keep }) }
    }

    /// Resolves once the provider has entered the held response.
    pub async fn entered(&self) {
        let mut receiver = self.inner.entered.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() { break; }
        }
    }

    /// Releases the held response.
    pub fn release(&self) {
        let _ = self.inner.released.send(true);
    }
}

/// A live, in-process session over the faux provider with the given native extensions registered.
pub struct NativeSession {
    session: maho_core::agent_session::AgentSession,
    runner: maho_ext_host::ExtensionRunner,
    provider: maho_ai::providers::faux::FauxProviderHandle,
    script: Vec<maho_ai::types::AssistantMessage>,
    calls: Arc<Mutex<Vec<ProviderCall>>>,
    temp: tempfile::TempDir,
}

impl NativeSession {
    /// Absolute temp cwd (files the tools write land here).
    pub fn cwd(&self) -> &Path {
        self.temp.path()
    }

    /// Runs the registered extension tool `name` exactly as the session would.
    pub async fn execute_tool(&self, name: &str, params: Value) -> Result<maho_agent::types::AgentToolResult, String> {
        self.session.execute_tool(name, params, maho_core::agent_session::ExecuteToolOptions::default())
            .await.map_err(|error| error.message)
    }

    /// Switches the active model, as `session.setModel` does.
    pub async fn set_model(&self, model: maho_ai::types::Model) -> Result<(), String> {
        self.session.set_model(model).await.map(|_| ())
    }

    /// Emits `before_provider_request` with the given payload and effective model and returns the
    /// resulting payload (the injector's output).
    pub async fn emit_before_provider_request(&self, payload: Value, model: Option<maho_ai::types::Model>) -> Result<Value, String> {
        let mut runner = self.runner.clone();
        runner.emit_before_provider_request_with_metadata(payload, model, None, None).await.map_err(|error| error.message)
    }

    /// Emits `before_agent_start` and returns the resulting system prompt.
    pub async fn emit_before_agent_start(&self, prompt: &str, system_prompt: &str) -> Result<Option<String>, String> {
        let mut runner = self.runner.clone();
        let combined = runner.emit_before_agent_start(maho_ext_api::BeforeAgentStartEvent {
            prompt: prompt.to_owned(), images: None, system_prompt: system_prompt.to_owned(),
            system_prompt_options: Default::default(),
        }).await.map_err(|error| error.message)?;
        Ok(combined.and_then(|combined| combined.system_prompt))
    }

    /// Emits `resources_discover` and returns the contributed skill paths.
    pub async fn discover_skills(&self) -> Result<Vec<String>, String> {
        let mut runner = self.runner.clone();
        let resources = runner.emit_resources_discover(self.session.cwd().into(), maho_ext_api::SessionReason::Startup)
            .await.map_err(|error| error.message)?;
        Ok(resources.skill_paths.into_iter().map(|entry| entry.path).collect())
    }

    /// Drives `session.prompt` for the concurrency cases and dispatches a leading slash command
    /// (e.g. `/btw`) through the registered command handlers. Returns a join handle so a second
    /// prompt can start while the first is held on a scripted gate.
    pub fn prompt(&self, text: String) -> tokio::task::JoinHandle<Result<(), String>> {
        let session = self.session.clone();
        tokio::spawn(async move {
            session.prompt(&text, maho_core::agent_session::PromptOptions::default()).await.map(|_| ())
        })
    }

    /// Scripts the next provider response to wait until the returned gate is released.
    pub fn hold_next_response(&self) -> ResponseGate {
        let gate = ResponseGate::new();
        let consumed = self.script.len().saturating_sub(self.provider.get_pending_response_count());
        let steps = self.script.iter().enumerate()
            .filter(|(index, _)| *index >= consumed)
            .map(|(index, message)| recording_step(message.clone(), self.calls.clone(), (index == consumed).then(|| gate.clone())))
            .collect();
        self.provider.set_responses(steps);
        gate
    }

    /// Every provider call the session made, in order (context + options).
    pub fn provider_calls(&self) -> Vec<ProviderCall> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The LLM messages the session recorded, in order (`AgentMessage::Llm` unwrapped).
    pub fn messages(&self) -> Vec<maho_ai::types::Message> {
        self.session.messages().into_iter().filter_map(|message| match message {
            maho_agent::types::AgentMessage::Llm(message) => Some(message),
            maho_agent::types::AgentMessage::Custom(_) => None,
        }).collect()
    }
}

fn recording_step(message: maho_ai::types::AssistantMessage, calls: Arc<Mutex<Vec<ProviderCall>>>,
    gate: Option<ResponseGate>) -> maho_ai::providers::faux::FauxResponseStep {
    maho_ai::providers::faux::FauxResponseStep::Factory(Arc::new(move |context, options, _state, _model| {
        let calls = calls.clone();
        let message = message.clone();
        let gate = gate.clone();
        let context = context.clone();
        let options = options.cloned();
        Box::pin(async move {
            calls.lock().unwrap_or_else(PoisonError::into_inner).push(ProviderCall { context, options });
            if let Some(gate) = gate {
                let _ = gate.inner.entered.send(true);
                let mut released = gate.inner.released.subscribe();
                while !*released.borrow_and_update() {
                    if released.changed().await.is_err() { break; }
                }
            }
            message
        })
    }))
}

pub fn run_faux_scenario(scenario: &str, omo: bool) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_out = std::env::temp_dir().join(format!("maho-faux-{}-{}.json", scenario, now));
    let repo_root = find_repo_root();
    let script_path = repo_root.join("tools/golden/faux-harness.mjs");

    let mut cmd = Command::new("bun");
    cmd.arg(&script_path)
        .arg("--scenario")
        .arg(scenario)
        .arg("--out")
        .arg(&temp_out);
    if omo {
        cmd.arg("--omo");
    }

    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("faux-harness generator failed with exit status {status}").into());
    }

    let data = std::fs::read_to_string(&temp_out)?;
    let _ = std::fs::remove_file(&temp_out);
    let json: Value = serde_json::from_str(&data)?;
    Ok(json)
}

fn find_repo_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    while !dir.join("tools/golden/faux-harness.mjs").exists() {
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        } else {
            return PathBuf::from("/home/indo/T9-Mac/maho-code");
        }
    }
    dir
}
