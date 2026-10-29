//! Concrete `FactsChildSessionSpawner`: the CLI's in-process facts extractor child.
//!
//! Mirrors upstream omo 5.1.35 ChildSpec:
//! taskId: facts-<runId>
//! cwd/sessionDir: runDir
//! toolAllowlist: [record_fact] (or [record_fact, record_agreement])
//! memberScopedTools: [record_fact tool] (and [record_agreement tool])
//! depth: 1
//! systemPrompt: facts persona
//! promptEnvelope: "bare"
//! completion: "turn"
//! prompt: "Extract durable facts from this payload and record each accepted fact with record_fact.\n\n<payload>"

use std::{
    sync::{
        atomic::Ordering,
        Arc, OnceLock,
    },
    time::Duration,
};

use maho_ai::types::StopReason;
use maho_omo_memory::facts_child_spawner::{
    FactsChildSessionResult, FactsChildSessionSpawner, FactsChildSpawnFuture, FactsChildSpawnInput,
};

pub enum FactsRuntimeFactory {
    Ready(maho_core::sdk::HostRuntimeFactory),
    Lazy(OnceLock<maho_core::sdk::HostRuntimeFactory>),
}

pub struct CliFactsChildSpawner {
    factory: FactsRuntimeFactory,
    agent_dir: String,
}

impl CliFactsChildSpawner {
    pub fn new(factory: FactsRuntimeFactory, agent_dir: String) -> Self {
        Self { factory, agent_dir }
    }

    pub fn from_agent_dir(agent_dir: String) -> Arc<Self> {
        let factory = FactsRuntimeFactory::Lazy(OnceLock::new());
        Arc::new(Self::new(factory, agent_dir))
    }

    pub fn with_factory(factory: FactsRuntimeFactory, agent_dir: String) -> Arc<Self> {
        Arc::new(Self::new(factory, agent_dir))
    }

    fn runtime_factory(&self) -> Result<&maho_core::sdk::HostRuntimeFactory, String> {
        let lazy = match &self.factory {
            FactsRuntimeFactory::Ready(factory) => return Ok(factory),
            FactsRuntimeFactory::Lazy(lazy) => lazy,
        };
        if let Some(factory) = lazy.get() {
            return Ok(factory);
        }
        let agent_dir = std::path::Path::new(&self.agent_dir);
        let model_runtime = maho_core::model_runtime::ModelRuntime::create_sync(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                models_path: Some(agent_dir.join("models.json")),
                auth_path: Some(agent_dir.join("auth.json")),
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(lazy.get_or_init(|| maho_core::sdk::HostRuntimeFactory {
            model_registry: maho_core::model_registry::ModelRegistry::new(model_runtime.clone()),
            model_runtime,
            extension_factories: Vec::new(),
        }))
    }
}

impl FactsChildSessionSpawner for CliFactsChildSpawner {
    fn spawn<'a>(&'a self, input: FactsChildSpawnInput) -> FactsChildSpawnFuture<'a> {
        Box::pin(async move {
            let factory = self.runtime_factory()?;

            let mut last_error: Option<String> = None;
            let mut attempt_number = 0u32;
            let mut last_model: Option<String> = None;
            let signal = input.signal.clone().unwrap_or_default();
            let cancel_state = Arc::clone(&input.cancel_state);

            for (idx, candidate) in input.model_candidates.iter().enumerate() {
                attempt_number = u32::try_from(idx + 1).unwrap_or(u32::MAX);
                last_model = Some(candidate.model.clone());

                let now_ms = memory_core::support::time::now_millis() as f64;
                if now_ms >= input.hard_deadline_at {
                    signal.abort();
                    cancel_state.store(true, Ordering::Release);
                    return Ok(FactsChildSessionResult {
                        completed: false,
                        timed_out: true,
                        cancelled: false,
                        error_message: Some("hard deadline reached".into()),
                        attempt: attempt_number,
                        model: last_model,
                    });
                }
                if signal.is_aborted()
                    || cancel_state.load(Ordering::Acquire)
                {
                    signal.abort();
                    cancel_state.store(true, Ordering::Release);
                    return Ok(FactsChildSessionResult {
                        completed: false,
                        timed_out: false,
                        cancelled: true,
                        error_message: Some("facts run cancelled".into()),
                        attempt: attempt_number,
                        model: last_model,
                    });
                }

                // Resolve provider and model id
                let (provider, id) = candidate
                    .model
                    .split_once('/')
                    .map(|(p, i)| (p.to_owned(), i.to_owned()))
                    .unwrap_or_else(|| (candidate.model.clone(), String::new()));

                let native_model = match factory.model_registry.find(&provider, &id) {
                    Some(model) => model,
                    None => {
                        // Retryable miss: ModelNotVisible -> try next candidate in chain
                        last_error = Some(format!(
                            "Model \"{}\" not found in registry",
                            candidate.model
                        ));
                        continue;
                    }
                };

                let thinking_selection = candidate.thinking.as_deref().and_then(|level| {
                    maho_ai::types::ModelThinkingLevel::parse(level).map(|level| {
                        maho_ai::types::ThinkingSelection {
                            level,
                            source: maho_ai::types::ThinkingSelectionSource::Explicit,
                            legacy_variant_id: None,
                        }
                    })
                });

                let retry_settings =
                    senpi_task::runners::in_process::runtime_fallback_settings::RetryFallbackSettings {
                        model_fallback: false,
                        chains: std::collections::BTreeMap::new(),
                        max_retries: Some(0),
                        base_delay_ms: None,
                    };
                let settings_manager = super::task_runners::native_child_settings(&retry_settings);

                let session_dir = input.run_dir.to_string_lossy().into_owned();
                let options = maho_core::sdk::CreateAgentSessionOptions {
                    cwd: Some(session_dir.clone()),
                    agent_dir: Some(self.agent_dir.clone()),
                    model: Some(native_model),
                    thinking_selection,
                    tools: Some(input.tool_allowlist.clone()),
                    custom_tools: input.custom_tools.clone(),
                    system_prompt: Some(input.persona.clone()),
                    settings_manager: Some(settings_manager),
                    session_manager: Some(maho_core::session_manager::SessionManager::create(
                        &session_dir,
                        Some(&session_dir),
                        None,
                    )),
                    minimal_resources: true,
                    ..Default::default()
                };

                let created = match factory.create(options).await {
                    Ok(created) => created,
                    Err(err) => {
                        let miss = maho_omo_memory::worker::model_miss::classify_retryable_model_miss(
                            &maho_omo_memory::worker::model_miss::ModelMissResult {
                                code: Some(1),
                                stdout: "",
                                stderr: &err,
                                timed_out: false,
                            },
                        );
                        if matches!(
                            miss,
                            Some(maho_omo_memory::worker::model_miss::RetryableModelMiss::AuthMissing { .. })
                                | Some(maho_omo_memory::worker::model_miss::RetryableModelMiss::ModelNotVisible { .. })
                        ) {
                            last_error = Some(err);
                            continue;
                        }
                        return Ok(FactsChildSessionResult {
                            completed: false,
                            timed_out: false,
                            cancelled: false,
                            error_message: Some(err),
                            attempt: attempt_number,
                            model: last_model,
                        });
                    }
                };

                created
                    .session
                    .set_system_prompt_sources(Some(input.persona.clone()), Vec::new());
                created.session.set_auto_compaction_enabled(false);
                let session = Arc::new(created.session);

                // Run one turn with deadline and cancellation enforcement
                let remaining_ms = (input.hard_deadline_at
                    - memory_core::support::time::now_millis() as f64)
                    .max(1.0) as u64;

                let prompt_fut = session.prompt(
                    &input.prompt,
                    maho_core::agent_session::PromptOptions::default(),
                );
                tokio::pin!(prompt_fut);
                let deadline_sleep = tokio::time::sleep(Duration::from_millis(remaining_ms));
                tokio::pin!(deadline_sleep);

                enum TurnExit {
                    PromptDone(Result<maho_core::agent_session::PromptDisposition, String>),
                    Deadline,
                    Aborted,
                }

                let exit = tokio::select! {
                    res = &mut prompt_fut => TurnExit::PromptDone(res),
                    _ = &mut deadline_sleep => TurnExit::Deadline,
                    () = signal.aborted() => TurnExit::Aborted,
                };

                match exit {
                    TurnExit::Deadline => {
                        signal.abort();
                        cancel_state.store(true, Ordering::Release);
                        // The turn only ends while prompt_fut is polled, so drive both together.
                        let ((), _) = tokio::join!(session.abort(), &mut prompt_fut);
                        session.dispose().await;
                        return Ok(FactsChildSessionResult {
                            completed: false,
                            timed_out: true,
                            cancelled: false,
                            error_message: Some("deadline exceeded".into()),
                            attempt: attempt_number,
                            model: last_model,
                        });
                    }
                    TurnExit::Aborted => {
                        signal.abort();
                        cancel_state.store(true, Ordering::Release);
                        // The turn only ends while prompt_fut is polled, so drive both together.
                        let ((), _) = tokio::join!(session.abort(), &mut prompt_fut);
                        session.dispose().await;
                        return Ok(FactsChildSessionResult {
                            completed: false,
                            timed_out: false,
                            cancelled: true,
                            error_message: Some("facts run cancelled".into()),
                            attempt: attempt_number,
                            model: last_model,
                        });
                    }
                    TurnExit::PromptDone(prompt_res) => {
                        let messages = session.messages();
                        session.dispose().await;
                        if signal.is_aborted()
                            || cancel_state.load(Ordering::Acquire)
                        {
                            signal.abort();
                            cancel_state.store(true, Ordering::Release);
                            return Ok(FactsChildSessionResult {
                                completed: false,
                                timed_out: false,
                                cancelled: true,
                                error_message: Some("facts run cancelled".into()),
                                attempt: attempt_number,
                                model: last_model,
                            });
                        }
                        let err = match prompt_res {
                            Ok(_) => {
                                let last_assistant = messages
                                    .iter()
                                    .rev()
                                    .find_map(|message| message.as_assistant());
                                if let Some(assistant) = last_assistant
                                    && matches!(
                                        assistant.stop_reason,
                                        StopReason::Error | StopReason::Aborted
                                    )
                                {
                                    let msg = assistant
                                        .error_message
                                        .as_deref()
                                        .map(str::trim)
                                        .filter(|m| !m.is_empty())
                                        .map(ToOwned::to_owned)
                                        .unwrap_or_else(|| {
                                            format!(
                                                "model turn ended with stop reason: {}",
                                                assistant.stop_reason.as_str()
                                            )
                                        });
                                    Some(msg)
                                } else {
                                    None
                                }
                            }
                            Err(err) => Some(err),
                        };

                        match err {
                            None => {
                                return Ok(FactsChildSessionResult {
                                    completed: true,
                                    timed_out: false,
                                    cancelled: false,
                                    error_message: None,
                                    attempt: attempt_number,
                                    model: last_model,
                                });
                            }
                            Some(err) => {
                                let miss =
                                    maho_omo_memory::worker::model_miss::classify_retryable_model_miss(
                                        &maho_omo_memory::worker::model_miss::ModelMissResult {
                                            code: Some(1),
                                            stdout: "",
                                            stderr: &err,
                                            timed_out: false,
                                        },
                                    );
                                if matches!(
                                    miss,
                                    Some(maho_omo_memory::worker::model_miss::RetryableModelMiss::AuthMissing { .. })
                                        | Some(maho_omo_memory::worker::model_miss::RetryableModelMiss::ModelNotVisible { .. })
                                ) {
                                    last_error = Some(err);
                                    continue;
                                }
                                return Ok(FactsChildSessionResult {
                                    completed: false,
                                    timed_out: false,
                                    cancelled: false,
                                    error_message: Some(err),
                                    attempt: attempt_number,
                                    model: last_model,
                                });
                            }
                        }
                    }
                }
            }

            Ok(FactsChildSessionResult {
                completed: false,
                timed_out: false,
                cancelled: false,
                error_message: last_error
                    .or_else(|| Some("all candidate models exhausted".into())),
                attempt: attempt_number,
                model: last_model,
            })
        })
    }
}
