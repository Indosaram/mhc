use std::path::PathBuf;
use std::process::Command;
use serde_json::Value;

use crate::faux::FauxScript;

pub struct FauxSession {
    pub scenario: String,
    pub omo: bool,
    pub script: FauxScript,
}

impl FauxSession {
    pub fn new(script: FauxScript) -> Self {
        Self {
            scenario: script.name.clone(),
            omo: false,
            script,
        }
    }

    pub fn with_extension(mut self, _ext: impl Into<String>) -> Self {
        self.omo = true;
        self
    }

    pub fn run_and_serialize(&self) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        run_faux_scenario(&self.scenario, self.omo)
    }

    pub async fn run_native(&self) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        use std::sync::{Arc, Mutex};
        use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider};
        use maho_core::agent_session::{AgentSession, AgentSessionConfig, PromptOptions};

        if self.omo {
            return Err("Native faux sessions require native extension registration; the bun golden runner remains available".into());
        }
        let temp = tempfile::tempdir()?;
        let cwd = temp.path().to_string_lossy().into_owned();
        let provider = faux_provider(RegisterFauxProviderOptions {
            api: Some("faux".to_owned()), tokens_per_second: Some(0.0), ..Default::default()
        });
        let model = provider.get_model(Some("faux-1")).ok_or("Missing faux-1 model")?;
        let responses = self.script.responses.iter().map(|response| {
            let stop_reason = serde_json::from_value::<maho_ai::types::StopReason>(Value::String(response.stop_reason.clone()))?;
            Ok(faux_assistant_message(response.content.clone(), FauxAssistantMessageOptions {
                stop_reason: Some(stop_reason), timestamp: Some(0), ..Default::default()
            }).into())
        }).collect::<Result<Vec<_>, serde_json::Error>>()?;
        provider.set_responses(responses);
        let streams = maho_ai::providers::faux::faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        });
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(temp.path().join("models.json")),
            auth_path: Some(temp.path().join("auth.json")),
            providers: Some(vec![provider.provider.clone()]), ..Default::default()
        });
        let session = AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
                stream_fn: Some(stream_fn), ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
            cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
            scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
            model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
            initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None,
            allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
            session_start_event: None, auto_title_sessions: Some(false),
        })?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            let value = match event {
                maho_ext_api::AgentSessionEvent::Agent(event) => serde_json::to_value(event).ok(),
                _ => None,
            };
            if let Some(value) = value {
                captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(value);
            }
        }));
        session.prompt(&self.script.prompt, PromptOptions::default()).await?;
        let messages = serde_json::to_value(session.messages())?;
        let entries = session.with_session_manager(|manager| manager.entries());
        let events = events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        session.dispose().await;
        Ok(serde_json::json!({ "scenario": self.scenario, "events": events, "entries": entries, "messages": messages }))
    }
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
