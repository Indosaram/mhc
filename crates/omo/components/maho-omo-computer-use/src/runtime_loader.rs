use std::sync::Arc;
use futures::future::BoxFuture;
use maho_ext_api::{ExecuteToolOptions, ExtensionFailure, ExtensionRuntime};
use senpi_desktop_core::error::PermissionDeniedData;
use senpi_desktop_service::child::ChildFactory;
use senpi_desktop_tool::cua_definition::COMPUTER_ACTIONS_TOOL_NAME;
use senpi_desktop_tool::settings::ComputerSettings;
use senpi_desktop_tool::tool_definition::COMPUTER_TOOL_NAME;
use serde_json::Value;

use crate::runtime::{create_computer_use_runtime, ComputerUseRuntime, ComputerUseRuntimeOptions};

pub trait ComputerHostApi: Send + Sync {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure>;
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure>;
    fn execute_tool(
        &self,
        name: &str,
        params: Value,
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> futures::future::BoxFuture<'static, Result<Value, String>>;
}

pub struct ExtensionRuntimeHostBridge {
    runtime: ExtensionRuntime,
}

impl ExtensionRuntimeHostBridge {
    pub fn new(runtime: ExtensionRuntime) -> Self {
        Self { runtime }
    }
}

impl ComputerHostApi for ExtensionRuntimeHostBridge {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        let actions = self.runtime.session_actions()?;
        actions.get_active_tools()
    }

    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        let actions = self.runtime.session_actions()?;
        actions.set_active_tools(names)
    }

    fn execute_tool(
        &self,
        name: &str,
        params: Value,
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> futures::future::BoxFuture<'static, Result<Value, String>> {
        let name_owned = name.to_string();
        let actions_res = self.runtime.session_actions();
        Box::pin(async move {
            let actions = actions_res.map_err(|e| e.to_string())?;
            let options = ExecuteToolOptions {
                signal,
                on_update: None,
                activate_inactive_tool: Some(true),
            };
            let fut = actions.execute_tool(&name_owned, params, options);
            let res = fut.await.map_err(|e| e.to_string())?;
            serde_json::to_value(&res).map_err(|e| e.to_string())
        })
    }
}

pub type EngineChildFactory = Arc<dyn Fn(Option<&str>) -> ChildFactory + Send + Sync>;
pub type EngineErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type PermissionCallback = Arc<dyn Fn(PermissionDeniedData) + Send + Sync>;
pub type RuntimeLoaderFn = Arc<dyn Fn() -> BoxFuture<'static, Result<ComputerUseRuntime, String>> + Send + Sync>;
pub type StartedRuntimeFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Arc<ComputerUseRuntime>, String>> + Send + 'static>>;

pub struct ComputerRuntimeLoaderOptions {
    pub host: Arc<dyn ComputerHostApi>,
    pub settings: ComputerSettings,
    pub engine_child: Option<EngineChildFactory>,
    pub on_engine_error: EngineErrorCallback,
    pub on_permission_required: Option<PermissionCallback>,
    pub load_runtime: Option<RuntimeLoaderFn>,
}

#[derive(Clone)]
enum LoaderState {
    Unstarted,
    Loading(tokio::sync::watch::Receiver<Option<Result<Arc<ComputerUseRuntime>, String>>>),
    Loaded(Arc<ComputerUseRuntime>),
}

pub struct ComputerRuntimeLoader {
    options: ComputerRuntimeLoaderOptions,
    state: Arc<std::sync::Mutex<LoaderState>>,
}

impl ComputerRuntimeLoader {
    pub fn new(options: ComputerRuntimeLoaderOptions) -> Self {
        Self {
            options,
            state: Arc::new(std::sync::Mutex::new(LoaderState::Unstarted)),
        }
    }

    pub fn options(&self) -> &ComputerRuntimeLoaderOptions {
        &self.options
    }

    pub fn is_started(&self) -> bool {
        let guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        !matches!(*guard, LoaderState::Unstarted)
    }

    pub fn started(&self) -> Option<StartedRuntimeFuture> {
        let guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*guard {
            LoaderState::Unstarted => None,
            LoaderState::Loaded(rt) => {
                let rt_clone = Arc::clone(rt);
                Some(Box::pin(std::future::ready(Ok(rt_clone))))
            }
            LoaderState::Loading(rx) => {
                let mut rx_clone = rx.clone();
                Some(Box::pin(async move {
                    if let Some(res) = rx_clone.borrow().as_ref() {
                        return res.clone();
                    }
                    rx_clone
                        .wait_for(|v| v.is_some())
                        .await
                        .map_err(|e| e.to_string())?;
                    rx_clone
                        .borrow()
                        .as_ref()
                        .expect("wait_for confirmed is_some")
                        .clone()
                }))
            }
        }
    }

    pub async fn load(&self) -> Result<Arc<ComputerUseRuntime>, String> {
        enum LoadAction {
            Loaded(Arc<ComputerUseRuntime>),
            Wait(tokio::sync::watch::Receiver<Option<Result<Arc<ComputerUseRuntime>, String>>>),
            Start(tokio::sync::watch::Sender<Option<Result<Arc<ComputerUseRuntime>, String>>>),
        }

        let action = {
            let mut guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*guard {
                LoaderState::Loaded(rt) => LoadAction::Loaded(Arc::clone(rt)),
                LoaderState::Loading(rx) => LoadAction::Wait(rx.clone()),
                LoaderState::Unstarted => {
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    *guard = LoaderState::Loading(rx);
                    LoadAction::Start(tx)
                }
            }
        };

        let tx = match action {
            LoadAction::Loaded(rt) => return Ok(rt),
            LoadAction::Wait(mut rx) => {
                if let Some(res) = rx.borrow().as_ref() {
                    return res.clone();
                }
                rx.wait_for(|v| v.is_some())
                    .await
                    .map_err(|e| e.to_string())?;
                return rx
                    .borrow()
                    .as_ref()
                    .expect("wait_for confirmed is_some")
                    .clone();
            }
            LoadAction::Start(tx) => tx,
        };

        let runtime_res = match &self.options.load_runtime {
            Some(loader_fn) => (loader_fn)().await,
            None => {
                let host_for_exec = Arc::clone(&self.options.host);
                let execute_tool: senpi_desktop_run::runtime::ExecuteTool = Arc::new(
                    move |tool_name: String, params: Value, signal: maho_ai::utils::abort::AbortSignal| {
                        let h = Arc::clone(&host_for_exec);
                        Box::pin(async move {
                            h.execute_tool(&tool_name, params, Some(signal)).await
                        })
                    },
                );

                Ok(create_computer_use_runtime(ComputerUseRuntimeOptions {
                    settings: self.options.settings.clone(),
                    execute_tool: Some(execute_tool),
                    engine_child: self.options.engine_child.clone(),
                    on_engine_error: Arc::clone(&self.options.on_engine_error),
                    on_permission_required: self.options.on_permission_required.clone(),
                }))
            }
        };

        match runtime_res {
            Ok(runtime) => {
                let host = Arc::clone(&self.options.host);
                let cua_adapter = self.options.settings.cua_adapter;
                let computer_tools = if cua_adapter {
                    vec![COMPUTER_TOOL_NAME.to_string(), COMPUTER_ACTIONS_TOOL_NAME.to_string()]
                } else {
                    vec![COMPUTER_TOOL_NAME.to_string()]
                };

                runtime.handle.on_activation_change(Arc::new(move |active: bool| {
                    let current = match host.get_active_tools() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let next = if active {
                        let mut next = current.clone();
                        for tool in &computer_tools {
                            if !next.contains(tool) {
                                next.push(tool.clone());
                            }
                        }
                        next
                    } else {
                        current.iter().filter(|t| !computer_tools.contains(t)).cloned().collect()
                    };
                    if next.len() == current.len() && next.iter().zip(current.iter()).all(|(a, b)| a == b) {
                        return;
                    }
                    let _ = host.set_active_tools(next);
                })).await;

                let arc_rt = Arc::new(runtime);
                {
                    let mut guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    *guard = LoaderState::Loaded(Arc::clone(&arc_rt));
                }
                let _ = tx.send(Some(Ok(Arc::clone(&arc_rt))));
                Ok(arc_rt)
            }
            Err(err) => {
                {
                    let mut guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    *guard = LoaderState::Unstarted;
                }
                let _ = tx.send(Some(Err(err.clone())));
                Err(err)
            }
        }
    }
}
