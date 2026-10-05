//! Port of senpi `experimental/mini/worker/run.ts` and `worker/entry.ts`.
//!
//! One process per session. It owns every live object — storage, harness, lane, model runtime — and
//! publishes them only as the `Lane` and `Models` services. It speaks JSON over its stdio pipes to
//! the server that spawned it. The harness is built through the pinned options constructor (S4) with
//! the read/write/edit/bash tools, the shared `ModelRuntime` models, the `ExecutionToolContext`, and
//! the `system_prompt(cwd)`; open operations left by a previous worker are re-driven through
//! `Lane::resume` (S3b).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use maho_agent::harness::agent_harness::AgentHarnessOptions;
use maho_agent::harness::context::{Context, BACKGROUND_CONTEXT};
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::runtime::harness::{create_agent_harness_with_options, Harness};
use maho_agent::harness::runtime::lane::Lane;
use maho_agent::harness::runtime::types::SystemPromptFn;
use maho_agent::harness::session::jsonl::{JsonlSessionCreateOptions, JsonlSessionRepo, JsonlSessionRepoOptions};
use maho_agent::harness::session::types::Session;
use maho_agent::harness::tools::{create_bash_tool, create_edit_tool, create_read_tool, create_write_tool, BashToolOptions, ExecutionToolContext, ReadToolOptions};
use maho_agent::harness::types::{AgentHarnessTool, ExecutionEnv, FileSystem};
use maho_core::model_resolver::{find_initial_model, InitialModelOptions};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use serde_json::Value;

use super::lane_service::{LaneService, LaneServiceOptions, SessionIdentity};
use super::models_service::ModelsService;
use super::runtime::ModelRuntimeHandle;
use super::shared::protocol::{LaneEvent, ModelRef, ModelsEvent, WorkerDescription, LANE, MODELS, WORKER};
use super::shared::rpc::{create_peer, Handler, PeerOptions};

pub fn system_prompt(cwd: &str) -> String {
    ["You are a coding agent working in a terminal.".to_owned(), format!("Working directory: {cwd}"), "Use the read, write, edit, and bash tools to inspect and change files.".to_owned(), "Keep answers short and technical.".to_owned()].join("\n")
}

pub async fn open_session(repo: &JsonlSessionRepo, session_id: Option<&str>, cwd: &str, context: &Context) -> Result<Box<dyn Session>, String> {
    match session_id {
        None => repo.create(JsonlSessionCreateOptions::new(cwd), context).await.map_err(|error| error.to_string()),
        Some(id) => {
            let metadata = repo.list(None, context).await.map_err(|error| error.to_string())?.into_iter().find(|metadata| metadata.id == id).ok_or_else(|| format!("Unknown session: {id}"))?;
            repo.open(metadata, context).await.map_err(|error| error.to_string())
        }
    }
}

async fn session_path(repo: &JsonlSessionRepo, id: &str, context: &Context) -> Result<String, String> {
    repo.list(None, context)
        .await
        .map_err(|error| error.message)?
        .into_iter()
        .find(|metadata| metadata.id == id)
        .map(|metadata| metadata.path)
        .ok_or_else(|| format!("Unknown session path: {id}"))
}

fn command_handler<F, Fut>(handler: F) -> Handler
where
    F: Fn(Vec<Value>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
{
    Arc::new(move |args, _signal| Box::pin(handler(args)))
}

fn text_argument(args: &[Value], method: &str) -> Result<String, String> {
    args.first().and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("{method} requires text"))
}

fn lane_handlers(service: Arc<LaneService>) -> HashMap<String, Handler> {
    let mut handlers = HashMap::new();
    {
        let service = service.clone();
        handlers.insert(
            "watch".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let presentation = args.first().and_then(Value::as_str).ok_or("lane.watch requires presentationId")?.to_owned();
                    let subscription = service.watch(&presentation).await?;
                    serde_json::to_value(subscription).map_err(|error| error.to_string())
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "start".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let subscription = args.first().and_then(Value::as_str).ok_or("lane.start requires subscriptionId")?.to_owned();
                    service.start(&subscription).await?;
                    Ok(Value::Null)
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "unwatch".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let subscription = args.first().and_then(Value::as_str).ok_or("lane.unwatch requires subscriptionId")?.to_owned();
                    service.unwatch(&subscription);
                    Ok(Value::Null)
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "prompt".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let text = text_argument(&args, "lane.prompt")?;
                    serde_json::to_value(service.prompt(&text).await).map_err(|error| error.to_string())
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "steer".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let text = text_argument(&args, "lane.steer")?;
                    serde_json::to_value(service.steer(&text).await).map_err(|error| error.to_string())
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "followUp".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let text = text_argument(&args, "lane.followUp")?;
                    serde_json::to_value(service.follow_up(&text).await).map_err(|error| error.to_string())
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "compact".to_owned(),
            command_handler(move |_args| {
                let service = service.clone();
                async move { serde_json::to_value(service.compact().await).map_err(|error| error.to_string()) }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "abort".to_owned(),
            command_handler(move |_args| {
                let service = service.clone();
                async move { serde_json::to_value(service.abort().await).map_err(|error| error.to_string()) }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "setModel".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let reference: ModelRef = serde_json::from_value(args.first().cloned().unwrap_or(Value::Null)).map_err(|error| error.to_string())?;
                    serde_json::to_value(service.set_model(&reference).await).map_err(|error| error.to_string())
                }
            }),
        );
    }
    handlers
}

fn models_handlers(service: Arc<ModelsService>) -> HashMap<String, Handler> {
    let mut handlers = HashMap::new();
    {
        let service = service.clone();
        handlers.insert(
            "refresh".to_owned(),
            command_handler(move |_args| {
                let service = service.clone();
                async move { serde_json::to_value(service.refresh().await).map_err(|error| error.to_string()) }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "login".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let provider = args.first().and_then(Value::as_str).ok_or("models.login requires providerId")?.to_owned();
                    let auth_type: super::shared::protocol::AuthType = serde_json::from_value(args.get(1).cloned().unwrap_or(Value::Null)).map_err(|error| error.to_string())?;
                    serde_json::to_value(service.login(&provider, auth_type).await).map_err(|error| error.to_string())
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "logout".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let provider = args.first().and_then(Value::as_str).ok_or("models.logout requires providerId")?.to_owned();
                    serde_json::to_value(service.logout(&provider).await).map_err(|error| error.to_string())
                }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "refresh".to_owned(),
            command_handler(move |_args| {
                let service = service.clone();
                async move { serde_json::to_value(service.refresh().await).map_err(|error| error.to_string()) }
            }),
        );
    }
    {
        let service = service.clone();
        handlers.insert(
            "authReply".to_owned(),
            command_handler(move |args| {
                let service = service.clone();
                async move {
                    let request_id = args.first().and_then(Value::as_str).ok_or("models.authReply requires requestId")?.to_owned();
                    let answer = args.get(1).and_then(Value::as_str).map(str::to_owned);
                    service.auth_reply(&request_id, answer);
                    Ok(Value::Null)
                }
            }),
        );
    }
    handlers
}

fn worker_handlers(session_id: String) -> HashMap<String, Handler> {
    let describe: Handler = Arc::new(move |_args, _signal| {
        let session_id = session_id.clone();
        Box::pin(async move { serde_json::to_value(WorkerDescription { session_id }).map_err(|error| error.to_string()) })
    });
    HashMap::from([("describe".to_owned(), describe)])
}

type WorkerHarness = Harness<ExecutionToolContext>;

fn worker_tools() -> Vec<Arc<AgentHarnessTool<ExecutionToolContext>>> {
    vec![
        Arc::new(create_read_tool::<ExecutionToolContext>(ReadToolOptions::default())),
        Arc::new(create_write_tool::<ExecutionToolContext>()),
        Arc::new(create_edit_tool::<ExecutionToolContext>()),
        Arc::new(create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())),
    ]
}

async fn open_harness(
    session: Arc<dyn Session>,
    model: &maho_ai::types::Model,
    thinking_level: maho_ai::types::ModelThinkingLevel,
    runtime: &ModelRuntimeHandle,
    env: Arc<NodeExecutionEnv>,
    cwd: &str,
    context: &Context,
) -> Result<WorkerHarness, String> {
    let tools = worker_tools();
    let execution_env: Arc<dyn ExecutionEnv> = env;
    let tool_context = ExecutionToolContext { env: execution_env, post_mutate: None };
    let prompt = system_prompt(cwd);
    let system_prompt_fn: SystemPromptFn = Arc::new(move |_context: &Context| {
        let prompt = prompt.clone();
        Box::pin(async move { prompt })
    });
    let options = AgentHarnessOptions::<ExecutionToolContext> {
        session,
        models: runtime.models().await,
        model: model.clone(),
        thinking_level: Some(thinking_level),
        active_tool_names: None,
        tools,
        tool_context: Some(tool_context),
        system_prompt: Some(system_prompt_fn),
        resources: None,
        stream_options: None,
        retry: None,
        compaction: None,
        steering_mode: None,
        follow_up_mode: None,
        tool_execution: None,
    };
    create_agent_harness_with_options(options, context).await.map(|(harness, _open)| harness).map_err(|error| error.message)
}

async fn lane_for(harness: &WorkerHarness, context: &Context) -> Result<Arc<Lane>, String> {
    harness.lane("main", None, context).await.map_err(|error| error.message)
}

async fn spawn_recoveries(harness: &WorkerHarness, context: &Context) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
    let mut recoveries = Vec::new();
    for operation in harness.open_operations().map_err(|error| error.message)? {
        let lane = harness.lane(&operation.lane, None, context).await.map_err(|error| error.message)?;
        let label = format!("{}/{}", operation.lane, operation.operation_id);
        let context = context.clone();
        recoveries.push(tokio::spawn(async move {
            match lane.resume(&context).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("Failed to resume {label}: {error}"),
                Err(error) => eprintln!("Failed to resume {label}: {}", error.message),
            }
        }));
    }
    Ok(recoveries)
}

pub struct SessionWorkerOptions {
    pub sessions_root: String,
    pub cwd: String,
    pub session_id: Option<String>,
}

pub async fn run_session_worker(options: SessionWorkerOptions) -> Result<(), String> {
    let context = BACKGROUND_CONTEXT.clone();
    let cwd = options.cwd.clone();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions::default()).await;
    let initial = find_initial_model(
        InitialModelOptions {
            cli_provider: None,
            cli_model: None,
            scoped_models: &[],
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            model_thinking_levels: None,
        },
        &runtime,
    )
    .await?;
    let model = initial.parsed.model.clone().ok_or("No model available. Configure credentials with `mhc` first.")?;
    let thinking_level = initial.parsed.thinking_level.unwrap_or(maho_ai::types::ModelThinkingLevel::Off);
    let runtime = ModelRuntimeHandle::new(runtime);

    let env = Arc::new(NodeExecutionEnv::new(&cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env.clone(), sessions_root: options.sessions_root.clone(), now: None });
    let session = open_session(&repo, options.session_id.as_deref(), &cwd, &context).await?;
    let session_id = session.metadata().id.clone();
    let path = session_path(&repo, &session_id, &context).await?;
    let session: Arc<dyn Session> = Arc::from(session);

    let harness = open_harness(session, &model, thinking_level, &runtime, env.clone(), &cwd, &context).await?;
    let lane = lane_for(&harness, &context).await?;

    let peer = Arc::new(create_peer(tokio::io::stdin(), tokio::io::stdout(), PeerOptions::default()));

    let publish_models: Arc<dyn Fn(ModelsEvent) + Send + Sync> = {
        let peer = peer.clone();
        Arc::new(move |event: ModelsEvent| peer.emit(MODELS, event.into()))
    };
    let models_service = Arc::new(ModelsService::new(runtime.clone(), publish_models).await);

    let publish_lane: Arc<dyn Fn(&str, &str, &maho_agent::harness::events::HarnessEvent) + Send + Sync> = {
        let peer = peer.clone();
        Arc::new(move |subscription_id: &str, to: &str, event: &maho_agent::harness::events::HarnessEvent| {
            let payload = LaneEvent { subscription_id: subscription_id.to_owned(), event: serde_json::Value::from(event) };
            peer.emit_to(LANE, serde_json::to_value(payload).unwrap_or(Value::Null), to);
        })
    };
    let lane_service = Arc::new(LaneService::new(LaneServiceOptions {
        lane: lane.clone(),
        models: runtime.clone(),
        context: context.clone(),
        session: SessionIdentity { id: session_id.clone(), cwd: cwd.clone(), path },
        models_state: {
            let models = models_service.clone();
            Arc::new(move || models.state())
        },
        publish: publish_lane,
    }));

    peer.provide(LANE, lane_handlers(lane_service.clone()));
    peer.provide(MODELS, models_handlers(models_service.clone()));
    peer.provide(WORKER, worker_handlers(session_id));

    let recoveries = spawn_recoveries(&harness, &context).await?;

    let (closed, closure) = tokio::sync::oneshot::channel();
    let closed = Mutex::new(Some(closed));
    peer.on_close(move || {
        if let Some(closed) = closed.lock().unwrap_or_else(|error| error.into_inner()).take() {
            let _ = closed.send(());
        }
    });
    let _ = closure.await;

    for recovery in recoveries {
        let _ = recovery.await;
    }
    lane_service.close();
    harness.close(&context).await;
    repo.close(&context).await;
    env.cleanup(&context).await;
    Ok(())
}
