pub mod bridge;
pub mod bridges;
pub mod config;
pub mod completion;
pub mod extension;
pub mod host_sdk;
pub mod interpreters;
pub mod kernels;
pub mod output;
pub mod prompt;
pub mod timeouts;
pub mod tool;

use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure, EventKind, EventResult};
use tool::{eval_tool_options::CreateEvalToolOptions, detached_cell_manager::{EvalDetachedCellManager, DetachedCellManagerOptions}};

pub type ContextCompletion = Arc<dyn Fn(completion::handler::CompletionRequest, ExtensionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<serde_json::Value,completion::handler::CompletionError>> + Send>> + Send + Sync>;

pub struct CodemodeExtensionOptions {
    pub image_sdk: Arc<dyn tool::image_resize::EvalImageSdk>,
    pub complete: ContextCompletion,
    pub home_dir: PathBuf,
    pub environment: config::settings::Environment,
    pub js_runtime: tool::types::EvalRuntimeInfo,
}

struct ExtensionState {
    runtime: Option<extension::runtime_factory::SessionRuntime>,
    cells: Arc<Mutex<EvalDetachedCellManager>>,
    context: Option<ExtensionContext>,
    model_id: Option<String>,
}

pub fn register(api: &mut ExtensionApi, options: CodemodeExtensionOptions) -> Result<(), ExtensionFailure> {
    let options=Arc::new(options);
    let proxy=Arc::new(extension::session_manager_proxy::SessionManagerProxy::default());
    let state=Arc::new(Mutex::new(ExtensionState {runtime:None,cells:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),context:None,model_id:None}));
    let host=Arc::new(Mutex::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone())));
    let ticker_context=Arc::downgrade(&state);
    let ticker=Arc::new(Mutex::new(extension::eval_status_ticker::EvalStatusTicker::new(Arc::new(move |status| {
        let context=ticker_context.upgrade().and_then(|state|state.lock().expect("extension state").context.clone());
        if let Some(context)=context {context.ui.set_status(extension::eval_status::EVAL_CELLS_STATUS_KEY,status.as_deref());}
    }),Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_secs_f64()*1000.0))));
    let notifier=Arc::new(Mutex::new(extension::eval_notifier::EvalNotifier::default()));
    let cells=create_extension_cells(&host,&state,&ticker,&notifier,&Default::default(),None);
    state.lock().expect("extension state").cells=cells;
    install_eval(api,&host,&state,&proxy,&options)?;
    api.register_removed_tool_hint("exec","exec was removed; use eval({ language: \"js\", code }) instead. Long eval cells detach on their own and notify when complete.");
    api.register_removed_tool_hint("wait","wait was removed; detached eval cells notify when complete. Use eval({ action: \"peek\"|\"stop\", cell_id }) to inspect or stop one.");
    host.lock().expect("extension api").registered=api.registered.clone();
    let version=(options.js_runtime.name=="bun").then(||options.js_runtime.version.clone());
    extension::skill_contribution::register_bun_skill_contribution(api,Arc::new(move ||version.clone()),PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"),std::env::current_exe().map_err(|error|ExtensionFailure::new(error.to_string()))?);
    let start_host=host.clone();let start_state=state.clone();let start_proxy=proxy.clone();let start_options=options.clone();let start_ticker=ticker.clone();let start_notifier=notifier.clone();
    api.on(EventKind::SessionStart,Arc::new(move |_,context| {
        let host=start_host.clone();let state=start_state.clone();let proxy=start_proxy.clone();let options=start_options.clone();let ticker=start_ticker.clone();let notifier=start_notifier.clone();
        Box::pin(async move {
            let previous=state.lock().expect("extension state").cells.clone();
            EvalDetachedCellManager::dispose(&previous).await.map_err(ExtensionFailure::new)?;
            let generation=proxy.begin_replacement();
            let mut detector=interpreters::detect::InterpreterDetector::new(options.js_runtime.version.clone(),cfg!(windows));
            let event=serde_json::json!({"sessionId":context.session_manager.session_id()});
            let mut prepared=extension::runtime_factory::prepare_runtime(extension::runtime_factory::RuntimePreparationOptions {cwd:&context.cwd,home_dir:&options.home_dir,environment:&options.environment,event:&event,session_file:context.session_manager.session_file(),js_runtime:options.js_runtime.clone()},&mut detector).await.map_err(|error|ExtensionFailure::new(error.to_string()))?;
            prepared.settings.foreground_window_seconds=config::settings::resolve_foreground_window_seconds(&prepared.settings,&options.environment);
            prepared.settings.run_budget_seconds=config::settings::resolve_run_budget_seconds(&prepared.settings,&options.environment);
            prepared.settings.hard_limit_seconds=config::settings::resolve_hard_limit_seconds(&prepared.settings,&options.environment);
            prepared.settings.max_detached_cells=config::settings::resolve_max_detached_cells(&prepared.settings,&options.environment);
            let captured=context.clone();let complete=options.complete.clone();
            let bridge_complete:bridge::http_server::BridgeCompletionHandler=Arc::new(move |request| {
                let context=captured.clone();let complete=complete.clone();
                Box::pin(async move {complete_with_signal(&complete,completion::handler::CompletionRequest {prompt:request.prompt,model:None,system:None,schema:None,opts:request.opts},context,request.signal).await.map_err(|error|serde_json::json!({"name":"Error","message":error.to_string()}))})
            });
            let runtime_api={let host=host.lock().expect("extension api");Arc::new(ExtensionApi::new(host.registered.clone(),host.profile.clone(),host.events.clone(),host.runtime.clone()))};
            let runtime_host=extension::runtime_factory::runtime_host_from_api(runtime_api,context,bridge_complete)?;
            let runtime=extension::runtime_factory::create_runtime(prepared,runtime_host).await.map_err(|error|ExtensionFailure::new(error.to_string()))?;
            if !proxy.replace(generation,runtime.manager.clone()).await {return Ok(EventResult::None);}
            notifier.lock().expect("notifier").reset();
            let cells=create_extension_cells(&host,&state,&ticker,&notifier,&runtime.settings,Some(runtime.artifacts_dir.clone()));
            {let mut state=state.lock().expect("extension state");state.context=Some(context.clone());state.model_id=context.model.as_ref().map(|model|model.id.clone());state.cells=cells.clone();state.runtime=Some(runtime);}
            cells.lock().expect("cell manager").publish_wake_source_state();
            let mut api=host.lock().expect("extension api");install_eval(&mut api,&host,&state,&proxy,&options)?;
            Ok(EventResult::None)
        })
    }));
    for event in [EventKind::SessionShutdown,EventKind::SessionBeforeSwitch,EventKind::SessionBeforeFork] {
        let state=state.clone();let proxy=proxy.clone();let ticker=ticker.clone();
        api.on(event,Arc::new(move |_,_| {let state=state.clone();let proxy=proxy.clone();let ticker=ticker.clone();Box::pin(async move {
            let cells={let mut state=state.lock().expect("extension state");state.runtime=None;state.model_id=None;state.cells.clone()};
            ticker.lock().expect("ticker").stop();
            let result=EvalDetachedCellManager::dispose(&cells).await;
            state.lock().expect("extension state").context=None;
            proxy.dispose().await;
            result.map_err(ExtensionFailure::new)?;Ok(EventResult::None)
        })}));
    }
    let model_host=host.clone();let model_state=state.clone();let model_proxy=proxy.clone();let model_options=options.clone();
    api.on(EventKind::ModelSelect,Arc::new(move |event,context| {
        let result=(|| {
            let mut state=model_state.lock().expect("extension state");state.context=Some(context.clone());
            let id=match event {maho_ext_api::ExtensionEvent::ModelSelect(event)=>Some(event.model.id.clone()),_=>None};
            if state.runtime.is_none() || id.is_none() || id==state.model_id {return Ok(());}
            state.model_id=id;drop(state);
            let mut api=model_host.lock().expect("extension api");install_eval(&mut api,&model_host,&model_state,&model_proxy,&model_options)
        })();
        Box::pin(async move {result?;Ok(EventResult::None)})
    }));
    Ok(())
}

fn create_extension_cells(host:&Arc<Mutex<ExtensionApi>>,state:&Arc<Mutex<ExtensionState>>,ticker:&Arc<Mutex<extension::eval_status_ticker::EvalStatusTicker>>,notifier:&Arc<Mutex<extension::eval_notifier::EvalNotifier>>,settings:&config::settings::CodemodeSettings,artifacts_dir:Option<PathBuf>) -> Arc<Mutex<EvalDetachedCellManager>> {
    let wake_host=Arc::downgrade(host);let notify_host=Arc::downgrade(host);let notify_state=Arc::downgrade(state);let notifier=notifier.clone();let ticker=ticker.clone();
    Arc::new(Mutex::new(EvalDetachedCellManager::new(DetachedCellManagerOptions {artifacts_dir,hard_limit_seconds:settings.hard_limit_seconds,run_budget_seconds:settings.run_budget_seconds,max_detached_cells:settings.max_detached_cells.ceil() as usize,
        on_status_change:Some(Arc::new(move |entries|ticker.lock().expect("ticker").sync(entries))),
        on_wake_source_state:Some(Arc::new(move |wake| {if let Some(host)=wake_host.upgrade() && let Err(error)=extension::wake_source_state::emit_wake_source_state(&host.lock().expect("extension api"),&wake) {eprintln!("codemode wake publication failed: {error}");}})),
        notifier:Some(Arc::new(move |cells| {let Some(host)=notify_host.upgrade() else {return Ok(());};let context=notify_state.upgrade().and_then(|state|state.lock().expect("extension state").context.clone());notifier.lock().expect("notifier").notify(&host.lock().expect("extension api"),context.as_ref(),extension::eval_notifier::EvalNotifyMode::Wake,&cells).map_err(|error|error.to_string())})),..Default::default()})))
}

fn install_eval(api:&mut ExtensionApi,host:&Arc<Mutex<ExtensionApi>>,state:&Arc<Mutex<ExtensionState>>,proxy:&Arc<extension::session_manager_proxy::SessionManagerProxy>,options:&Arc<CodemodeExtensionOptions>) -> Result<(),ExtensionFailure> {
    let settled_state=Arc::downgrade(state);
    let state=state.lock().expect("extension state");
    let runtime=&state.runtime;
    let mut settings=runtime.as_ref().map_or_else(config::settings::CodemodeSettings::default,|runtime|runtime.settings.clone());
    settings.foreground_window_seconds=config::settings::resolve_foreground_window_seconds(&settings,&options.environment);
    settings.run_budget_seconds=config::settings::resolve_run_budget_seconds(&settings,&options.environment);
    settings.hard_limit_seconds=config::settings::resolve_hard_limit_seconds(&settings,&options.environment);
    if let Some(runtime)=runtime {settings.languages=runtime.enabled_languages.clone();} else {settings.languages=config::settings::Languages {py:true,js:true,rb:true,jl:true};}
    let catalog_host=Arc::downgrade(host);let catalog=Arc::new(move ||catalog_host.upgrade().ok_or_else(||"codemode session manager is disposed".to_owned())?.lock().expect("extension api").get_all_tools().map(|tools|tools.into_iter().map(|tool|bridges::schema_bridge::EvalSchemaToolInfo {name:tool.name,description:Some(tool.description),parameters:Some(tool.parameters)}).collect()).map_err(|error|error.to_string()));
    let executor=runtime.as_ref().map_or_else(|| {let api=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));Arc::new(extension::runtime_factory::RuntimeExecuteTool {api,active_tools:vec![]}) as Arc<dyn bridges::output_bridge::OutputExecuteTool>},|runtime|runtime.executor.clone());
    let cells=state.cells.clone();let weak_cells=Arc::downgrade(&cells);
    let complete=options.complete.clone();let completion_context=state.context.clone();
    let completion:tool::cell_handler::CellCompletionHandler=Arc::new(move |request,signal| {let complete=complete.clone();let context=completion_context.clone();Box::pin(async move {let context=context.ok_or_else(||completion::handler::CompletionError("codemode session has not started".into()))?;complete_with_signal(&complete,request,context,signal).await})});
    let events=api.events.clone();
    let on_settled=Arc::new(move |payload:serde_json::Value| {if let (Some(state),Some(cells))=(settled_state.upgrade(),weak_cells.upgrade()) {let current=Arc::ptr_eq(&state.lock().expect("extension state").cells,&cells);if current {events.emit(tool::eval_execution_event::EVAL_EXECUTION_EVENT,&payload);events.emit("senpi:extension-rpc-event",&serde_json::json!({"name":tool::eval_execution_event::EVAL_EXECUTION_EVENT,"data":tool::eval_execution_event::to_eval_execution_rpc_payload(&payload)}));}}});
    let prompt=prompt::eval_prompt::EvalPromptOptions {model_id:state.model_id.clone(),spawns:runtime.as_ref().is_some_and(|runtime|runtime.spawns),monitor:runtime.is_some() && api.get_all_tools()?.iter().any(|tool|tool.name=="monitor"),spawn_default_agent:Some(settings.task_tools.task.clone()),js_runtime:Some(options.js_runtime.clone()),..Default::default()};
    let definition=tool::eval_tool::create_eval_tool(Arc::new(CreateEvalToolOptions {kernel_manager:proxy.clone(),executor,list_tools:Some(catalog),complete:Some(completion),settings,artifacts_dir:runtime.as_ref().map(|runtime|runtime.artifacts_dir.clone()),image_sdk:options.image_sdk.clone(),cell_manager:cells,on_cell_settled:Some(on_settled),prompt,runtimes:runtime.as_ref().map_or_else(HashMap::new,|runtime|runtime.runtimes.iter().cloned().collect()),mode:state.context.as_ref().map_or("print",|context|if matches!(context.mode,maho_ext_api::ExtensionMode::Print|maho_ext_api::ExtensionMode::Json) {"print"} else {"interactive"}).into()})).map_err(ExtensionFailure::new)?;
    api.try_register_tool(definition)
}

async fn complete_with_signal(complete:&ContextCompletion,request:completion::handler::CompletionRequest,mut context:ExtensionContext,signal:maho_ai::utils::abort::AbortSignal) -> Result<serde_json::Value,completion::handler::CompletionError> {
    let cancellation=maho_ext_api::AbortSignal::default();
    context.signal=Some(cancellation.clone());
    if signal.aborted() {cancellation.abort();}
    let operation=complete(request,context);
    tokio::pin!(operation);
    tokio::select! {
        result=&mut operation=>result,
        ()=signal.cancelled()=>{cancellation.abort();operation.await}
    }
}
