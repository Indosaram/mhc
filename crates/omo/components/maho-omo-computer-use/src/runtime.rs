use std::sync::Arc;
use maho_tools::definition::ToolDefinition;
use senpi_desktop_core::error::PermissionDeniedData;
use senpi_desktop_run::runtime::ExecuteTool;
use senpi_desktop_service::child::ChildFactory;
use senpi_desktop_service::service::DesktopServiceOptions;
use senpi_desktop_tool::activation::ComputerHandle;
use senpi_desktop_tool::cua_adapter::create_computer_actions_tool;
use senpi_desktop_tool::settings::ComputerSettings;
use senpi_desktop_tool::tool::{create_computer_tool, ComputerToolDeps};

use crate::engine_status::{TrackedDesktopService, TrackedDesktopServiceOptions};

pub type EngineChildFactory = Arc<dyn Fn(Option<&str>) -> ChildFactory + Send + Sync>;
pub type EngineErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type PermissionCallback = Arc<dyn Fn(PermissionDeniedData) + Send + Sync>;

pub struct ComputerUseRuntimeOptions {
    pub settings: ComputerSettings,
    pub execute_tool: Option<ExecuteTool>,
    pub engine_child: Option<EngineChildFactory>,
    pub on_engine_error: EngineErrorCallback,
    pub on_permission_required: Option<PermissionCallback>,
}

pub struct ComputerUseRuntime {
    pub handle: Arc<ComputerHandle>,
    pub service: Arc<TrackedDesktopService>,
    pub computer_tool: ToolDefinition,
    pub computer_actions_tool: Option<ToolDefinition>,
}

pub fn create_computer_use_runtime(options: ComputerUseRuntimeOptions) -> ComputerUseRuntime {
    let settings = options.settings.clone();
    let engine_path = settings.engine_path.clone();

    let child_factory = match options.engine_child {
        Some(factory) => Some(factory(engine_path.as_deref())),
        None => {
            let config = engine_path.map(|p| senpi_desktop_service::child::EngineSpawnConfig::new(std::path::PathBuf::from(p)));
            Some(senpi_desktop_service::child::engine_child_factory(config))
        }
    };

    let on_err = options.on_engine_error;
    let service = Arc::new(TrackedDesktopService::new(TrackedDesktopServiceOptions {
        service_options: DesktopServiceOptions {
            create_child: child_factory,
            on_notification: None,
        },
        on_error: Some(on_err),
        on_permission_required: options.on_permission_required,
    }));

    let handle_service = service.service();
    let settings_for_handle = settings.clone();
    let handle = Arc::new(ComputerHandle::new(
        handle_service,
        move || settings_for_handle.clone(),
    ));

    let deps = ComputerToolDeps {
        handle: Arc::clone(&handle),
        execute_tool: options.execute_tool,
    };

    let computer_tool = create_computer_tool(deps.clone());
    let computer_actions_tool = if settings.cua_adapter {
        Some(create_computer_actions_tool(deps))
    } else {
        None
    };

    ComputerUseRuntime {
        handle,
        service,
        computer_tool,
        computer_actions_tool,
    }
}
