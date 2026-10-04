//! Port of `omo-senpi/src/extension/compose.ts` at pin `77f3067f1`: the native composition that
//! provisions the environment, registers the `maho-omo-disabled` flag and every per-component
//! disabled flag, installs the shared coordinator/logger/macrotask runtime, and registers each
//! component in upstream order.
//!
//! Divergences (ledger N/A reasons):
//! - upstream checks `pi` for the required capabilities and disables the extension on a version
//!   mismatch; the native `ExtensionApi` is statically typed, so that branch has no native
//!   counterpart.
//! - upstream registers every component through one `pi`; native components are `Box<dyn Extension>`
//!   registered on the same `ExtensionApi`, which is the same single-identity model.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{
    ComponentLogger, DeliverAs, EventBus, Extension, ExtensionApi, ExtensionSessionProfile, FlagType, FlagValue,
    IdleInjectionCoordinator, LoadedExtension, SendMessageOptions, SourceInfo,
};

use crate::coordinator::{Delivery, IdleInjectionDelivery, IdleInjectionMessage};
use crate::logger::StderrLogger;
use crate::provisioning::{
    ProvisioningOptions, provision_dag_sdk_root, provision_toolkit_path,
};
use crate::runtime::{ConfigAccessor, OmoRuntime, OmoRuntimeOptions};

/// Upstream `omo-senpi-disabled`, rebranded per the plan (`omo-senpi-disabled -> maho-omo-disabled`).
pub const OMO_DISABLED_FLAG: &str = "maho-omo-disabled";
/// Upstream `omo-senpi-${name}-disabled`; only the top-level flag is rebranded.
pub const OMO_COMPONENT_FLAG_PREFIX: &str = "omo-senpi-";
pub const OMO_EXTENSION_IDENTITY: &str = "omo";

pub fn component_disabled_flag(name: &str) -> String {
    format!("{OMO_COMPONENT_FLAG_PREFIX}{name}-disabled")
}

/// One entry of the registration list. Holds a register closure so a component that is not a plain
/// `Box<dyn Extension>` (the memory component registers through `MemoryComponent::register` with
/// hooks) can still be a list entry without an invented wrapper type.
///
/// `register` may run more than once: `ExtensionRunner::recreate` (agent_session reload) re-registers
/// the SAME extension objects, so a slot whose extension consumes one-shot state must be built with
/// [`OmoSenpiComponent::from_factory`] to get a fresh instance per register.
pub struct OmoSenpiComponent {
    pub name: &'static str,
    register: Arc<dyn Fn(&mut ExtensionApi, &OmoRuntime) + Send + Sync>,
}

impl std::fmt::Debug for OmoSenpiComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OmoSenpiComponent").field("name", &self.name).finish_non_exhaustive()
    }
}

impl OmoSenpiComponent {
    pub fn new(name: &'static str, extension: Box<dyn Extension>) -> Self {
        Self { name, register: Arc::new(move |api, _| extension.register(api)) }
    }

    pub fn from_register(name: &'static str, register: impl Fn(&mut ExtensionApi) + Send + Sync + 'static) -> Self {
        Self { name, register: Arc::new(move |api, _| register(api)) }
    }

    /// Builds a fresh extension on every register, so a component whose registration consumes
    /// one-shot state still registers after a reload/recreate.
    pub fn from_factory(name: &'static str, factory: impl Fn() -> Box<dyn Extension> + Send + Sync + 'static) -> Self {
        Self { name, register: Arc::new(move |api, _| factory().register(api)) }
    }

    pub fn from_context_register(
        name: &'static str,
        register: impl Fn(&mut ExtensionApi, &OmoRuntime) + Send + Sync + 'static,
    ) -> Self {
        Self { name, register: Arc::new(register) }
    }

    pub fn register(&self, api: &mut ExtensionApi, runtime: &OmoRuntime) {
        (self.register)(api, runtime);
    }
}

pub struct OmoExtension {
    components: Vec<OmoSenpiComponent>,
    options: OmoRuntimeOptions,
    provisioning: ProvisioningOptions,
    runtime: Mutex<Option<Arc<OmoRuntime>>>,
    retained_runtime: Mutex<Option<Arc<OmoRuntime>>>,
    environment: Mutex<std::collections::BTreeMap<String, String>>,
}

impl OmoExtension {
    pub fn new(components: Vec<OmoSenpiComponent>) -> Self {
        Self::with_options(components, OmoRuntimeOptions::default(), ProvisioningOptions::default())
    }

    pub fn with_options(components: Vec<OmoSenpiComponent>, options: OmoRuntimeOptions, provisioning: ProvisioningOptions) -> Self {
        Self { components, options, provisioning, runtime: Mutex::new(None), retained_runtime: Mutex::new(None), environment: Mutex::new(std::collections::BTreeMap::new()) }
    }

    pub fn components(&self) -> &[OmoSenpiComponent] {
        &self.components
    }

    /// The runtime built during `register`; the CLI binds it into the session's `ExtensionContext`.
    pub fn runtime(&self) -> Option<Arc<OmoRuntime>> {
        self.runtime.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn environment(&self) -> std::collections::BTreeMap<String, String> {
        self.environment.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Extension for OmoExtension {
    fn register(&self, api: &mut ExtensionApi) {
        *self.runtime.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let logger: Arc<dyn ComponentLogger> = self.options.logger.clone().unwrap_or_else(|| Arc::new(StderrLogger));

        let updates = provision_toolkit_path(&self.provisioning).into_iter()
            .chain(provision_dag_sdk_root(&self.provisioning));
        let mut environment = self.environment.lock().unwrap_or_else(PoisonError::into_inner);
        environment.clear();
        for (name, value) in updates {
            if let Some(value) = value { environment.insert(name, value); }
        }
        drop(environment);

        api.register_flag(
            OMO_DISABLED_FLAG,
            FlagType::Boolean { default: Some(false) },
            Some("Disable all maho-omo components.".to_owned()),
        );
        for component in &self.components {
            api.register_flag(
                &component_disabled_flag(component.name),
                FlagType::Boolean { default: Some(false) },
                Some(format!("Disable the omo-senpi {} component.", component.name)),
            );
        }

        if is_true(api.get_flag(OMO_DISABLED_FLAG)) {
            logger.info("maho-omo disabled by flag", None);
            return;
        }

        let runtime = {
            let mut retained = self.retained_runtime.lock().unwrap_or_else(PoisonError::into_inner);
            match retained.as_ref() {
                Some(runtime) => {
                    runtime.rebind(delivery_from(api), config_from(api));
                    Arc::clone(runtime)
                }
                None => {
                    let runtime = Arc::new(OmoRuntime::new(delivery_from(api), config_from(api), self.options.clone()));
                    *retained = Some(Arc::clone(&runtime));
                    runtime
                }
            }
        };
        *self.runtime.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&runtime));
        let _turn = runtime.enter_turn();
        let existing: std::collections::BTreeMap<_, _> = api.registered.handlers.iter()
            .map(|(kind, handlers)| (*kind, handlers.len())).collect();

        for component in &self.components {
            if is_true(api.get_flag(&component_disabled_flag(component.name))) {
                let details = serde_json::json!({ "component": component.name });
                runtime.logger().info("omo-senpi component disabled by flag", Some(&details));
                continue;
            }
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| component.register(api, &runtime))) {
                let details = serde_json::json!({
                    "component": component.name,
                    "error": panic_reason(&payload),
                });
                runtime.logger().error("omo-senpi component registration failed", Some(&details));
            }
            runtime.capture_tools(api.registered.tools.iter().map(|tool| tool.definition.clone()).collect());
        }
        // Bind only this composition's handlers, leaving earlier host handlers untouched.
        // The guard spans the awaited handler: no deferred queue pass splits its turn.
        for (kind, handlers) in &mut api.registered.handlers {
            for handler in handlers.iter_mut().skip(existing.get(kind).copied().unwrap_or(0)) {
                let original = Arc::clone(handler);
                let shared = Arc::clone(&runtime);
                *handler = Arc::new(move |event, context| {
                    let original = Arc::clone(&original);
                    let shared = Arc::clone(&shared);
                    let mut context = context.clone();
                    shared.bind(&mut context);
                    Box::pin(async move {
                        let _turn = shared.enter_turn();
                        original(event, &context).await
                    })
                });
            }
        }

        runtime.capture_tools(api.registered.tools.iter().map(|tool| tool.definition.clone()).collect());
        *self.runtime.lock().unwrap_or_else(PoisonError::into_inner) = Some(runtime);
    }
}

fn is_true(flag: Option<FlagValue>) -> bool {
    matches!(flag, Some(FlagValue::Boolean(true)))
}

fn config_from(api: &ExtensionApi) -> ConfigAccessor {
    let runtime = api.runtime.clone();
    Arc::new(move |name: &str| runtime.get_flag(name))
}

/// Upstream `(message, options) => pi.sendMessage(message, { triggerTurn: true, deliverAs })`.
fn delivery_from(api: &ExtensionApi) -> IdleInjectionDelivery {
    let runtime = api.runtime.clone();
    let cwd = api.cwd.clone();
    Arc::new(move |message: &IdleInjectionMessage, deliver_as: DeliverAs| {
        let api = ExtensionApi::new(
            LoadedExtension::new(OMO_EXTENSION_IDENTITY, cwd.clone(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime.clone(),
        );
        match api.send_message(
            message.to_custom_message(),
            SendMessageOptions { trigger_turn: true, deliver_as: Some(deliver_as) },
        ) {
            Ok(()) => Delivery::Delivered,
            Err(error) => Delivery::Failed(error.to_string()),
        }
    })
}

fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "component registration panicked".to_owned()
    }
}

/// Upstream `composeOmoSenpiExtension(components)` -> the native extension entry.
pub fn compose_omo_extension(components: Vec<OmoSenpiComponent>) -> OmoExtension {
    OmoExtension::new(components)
}

pub fn compose_omo_extension_with_options(
    components: Vec<OmoSenpiComponent>,
    options: OmoRuntimeOptions,
    provisioning: ProvisioningOptions,
) -> OmoExtension {
    OmoExtension::with_options(components, options, provisioning)
}

/// Convenience for consumers that only need the shared coordinator trait object.
pub fn coordinator_of(runtime: &OmoRuntime) -> Arc<dyn IdleInjectionCoordinator> {
    Arc::new(runtime.coordinator())
}
