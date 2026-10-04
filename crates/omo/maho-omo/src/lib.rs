//! Native OMO extension entry and shared host composition (todo 47 residual).

pub mod capture;
pub mod component_list;
pub mod compose;
pub mod coordinator;
pub mod entry;
pub mod logger;
pub mod provisioning;
pub mod runtime;
pub mod scheduler;
pub mod tool_hook_status;
pub mod task_coordinator;
pub use task_coordinator::TaskCoordinator;

pub use capture::ToolCaptureRegistry;
pub use component_list::{
    MissingComponents, OmoComponentOptions, SKILLS_ROOT_ENV, builtin_skills_root, memory_component_from,
    omo_component_names, omo_components, try_omo_components,
};
pub use compose::{
    OMO_COMPONENT_FLAG_PREFIX, OMO_DISABLED_FLAG, OMO_EXTENSION_IDENTITY, OmoExtension, OmoSenpiComponent,
    component_disabled_flag, compose_omo_extension, compose_omo_extension_with_options, coordinator_of,
};
pub use coordinator::{
    DETAIL_SEPARATOR, Delivery, DeliveryFuture, IdleInjectionDelivery, IdleInjectionDetail, IdleInjectionMessage,
    IdleInjectionQueue, WAKE_CUSTOM_TYPE,
};
pub use entry::{bundled_omo_senpi_extension, omo_senpi_extension, omo_task_component, try_omo_senpi_extension};
pub use logger::{LogEntry, LogLevel, LogSink, RecordingLogger, SinkLogger, StderrLogger};
pub use provisioning::{
    DAG_SDK_ROOT_ENV, EnvReader, EnvWriter, ProvisioningOptions, TOOLKIT_BIN_ENV, dag_sdk_base_dir_default,
    path_delimiter, provision_dag_sdk_root, provision_toolkit_path, toolkit_base_dir_default,
};
pub use runtime::{BATCH_WINDOW, ConfigAccessor, OmoComponentContext, OmoRuntime, OmoRuntimeOptions};
pub use scheduler::{
    DeferredScheduler, DeferredTask, SpawnSeam, TurnBarrier, TurnGuard, runtime_macrotask, runtime_microtask,
};
pub use tool_hook_status::report_tool_hook_status;
