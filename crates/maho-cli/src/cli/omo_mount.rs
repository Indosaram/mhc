//! Port of the host half of senpi `omo-senpi/src/index.ts`.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use maho_core::agent_session::AgentSession;
use maho_ext_api::{Extension, ExtensionApi, ExtensionSessionProfile, ExtensionUi};
use maho_ext_host::loader::{load_extensions, LoadExtensionsResult, NativeExtensionFactory};
use maho_ext_host::ExtensionRunner;
use maho_omo::{
    OMO_EXTENSION_IDENTITY, OmoComponentOptions, OmoExtension, OmoRuntime, OmoRuntimeOptions,
    OmoSenpiComponent, ProvisioningOptions, memory_component_from, omo_senpi_extension,
};

pub struct OmoMount {
    extension: Arc<OmoExtension>,
}

impl OmoMount {
    pub fn shipped(
        task: OmoSenpiComponent,
        memory: OmoSenpiComponent,
        components: &OmoComponentOptions,
        runtime: OmoRuntimeOptions,
        provisioning: ProvisioningOptions,
    ) -> Self {
        Self { extension: Arc::new(omo_senpi_extension(task, memory, components, runtime, provisioning)) }
    }

    pub fn extension(&self) -> &Arc<OmoExtension> {
        &self.extension
    }

    pub fn runtime(&self) -> Option<Arc<OmoRuntime>> {
        self.extension.runtime()
    }

    pub fn factory(&self) -> NativeExtensionFactory {
        let path = format!("<builtin:{OMO_EXTENSION_IDENTITY}>");
        NativeExtensionFactory {
            path: path.clone(),
            source_info: maho_ext_api::SourceInfo { path, source: "builtin".to_owned(), ..Default::default() },
            extension: Box::new(SharedOmoExtension(Arc::clone(&self.extension))),
        }
    }
}

pub fn memory_entry(build: impl Fn() -> maho_omo_memory::composition::MemoryExtensionOptions + Send + Sync + 'static) -> OmoSenpiComponent {
    memory_component_from(build)
}

pub fn task_component(
    engine: maho_omo_task::engine::TaskEngine,
    spawn: senpi_task::tools::task::execute_spec::TaskToolDeps,
    ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
) -> OmoSenpiComponent {
    let state = Arc::new(Mutex::new(Some((engine, spawn, ownership))));
    OmoSenpiComponent::from_register("task", move |api| {
        let Some((engine, spawn, ownership)) = state.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return;
        };
        if let Err(error) = maho_omo_task::component::TaskComponent::register(api, engine, spawn, ownership, false) {
            eprintln!("omo task component registration failed: {}", error.message);
        }
    })
}

struct SharedOmoExtension(Arc<OmoExtension>);

impl Extension for SharedOmoExtension {
    fn register(&self, api: &mut ExtensionApi) {
        self.0.register(api);
    }
}

pub fn bind_runtime_into_context(runtime: &OmoRuntime, context: &mut maho_ext_api::ExtensionContext) {
    runtime.bind(context);
}

/// senpi's noninteractive `noOpUIContext`; dialogs have no answer and widgets have no surface.
pub struct NoninteractiveUi;

impl ExtensionUi for NoninteractiveUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, serde_json::Value> { Box::pin(async { Ok(serde_json::Value::Null) }) }
    fn theme(&self) -> maho_ext_api::Theme { Default::default() }
}

pub async fn mount_base_extensions(
    session: &AgentSession,
    ui: Arc<dyn ExtensionUi>,
    base: Vec<NativeExtensionFactory>,
) -> Result<LoadExtensionsResult, String> {
    let cwd = session.cwd();
    let loaded = load_extensions(base, Path::new(&cwd), ExtensionSessionProfile::default());
    bind_loaded_extensions(session, ui, &loaded, None).await?;
    Ok(loaded)
}

pub async fn bind_loaded_extensions(
    session: &AgentSession,
    ui: Arc<dyn ExtensionUi>,
    loaded: &LoadExtensionsResult,
    omo: Option<&OmoMount>,
) -> Result<(), String> {
    let mut context = session.extension_context(ui);
    if let Some(runtime) = omo.and_then(OmoMount::runtime) {
        bind_runtime_into_context(&runtime, &mut context);
    }
    let runner = ExtensionRunner::new(loaded.extensions.clone(), loaded.runtime.clone(), loaded.events.clone(), context);
    session.set_extension_runner(runner).await;
    Ok(())
}

pub async fn mount_native_extensions_with_omo(
    session: &AgentSession,
    ui: Arc<dyn ExtensionUi>,
    base: Vec<NativeExtensionFactory>,
    omo: &OmoMount,
) -> Result<LoadExtensionsResult, String> {
    let mut factories = base;
    factories.push(omo.factory());
    let cwd = session.cwd();
    let loaded = load_extensions(factories, Path::new(&cwd), ExtensionSessionProfile::default());
    let mut context = session.extension_context(ui);
    if let Some(runtime) = omo.runtime() {
        bind_runtime_into_context(&runtime, &mut context);
    }
    let runner = ExtensionRunner::new(loaded.extensions.clone(), loaded.runtime.clone(), loaded.events.clone(), context);
    session.set_extension_runner(runner).await;
    Ok(loaded)
}
