//! Native gateway rules component factory and registration.
//!
//! Upstream counterpart: `index.ts`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use rusqlite::Connection;
use serde_json::Value;

use maho_ext_api::{
    BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent,
};
use maho_omo_agent_home::resolve_agent_home;
use omo_config_core::LoadOmoConfigOptions;
use maho_omo_config_resolution::load_senpi_omo_config;

use crate::paths::gateway_database_path;
use crate::prompt::{
    GatewayRulesPromptHandler, GatewayRulesPromptOptions, GatewayRulesStore,
    SqliteGatewayRulesStore,
};
use crate::store_extension::apply_gateway_rules_migrations;

pub type AgentDirResolver = Arc<dyn Fn() -> PathBuf + Send + Sync>;
pub type StoreCreator = Arc<dyn Fn(&Path) -> Result<Arc<dyn GatewayRulesStore>, String> + Send + Sync>;
pub type GatewaySectionLoader = Arc<dyn Fn() -> Option<Value> + Send + Sync>;

/// Options for configuring the gateway component.
#[derive(Default)]
pub struct GatewayComponentOptions {
    pub agent_dir: Option<AgentDirResolver>,
    pub create_store: Option<StoreCreator>,
    /// The raw `gateway` config section; a non-empty `scopes` array activates the component.
    pub load_gateway_section: Option<GatewaySectionLoader>,
}

pub struct GatewayComponent {
    options: GatewayComponentOptions,
}

impl GatewayComponent {
    pub const NAME: &str = "gateway";

    pub fn new(options: GatewayComponentOptions) -> Self {
        Self { options }
    }
}

impl Extension for GatewayComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let store_holder: Arc<Mutex<Option<Arc<dyn GatewayRulesStore>>>> = Arc::new(Mutex::new(None));
        let settled = Arc::new(AtomicBool::new(false));
        let registration_warned = Arc::new(AtomicBool::new(false));

        let opt_load_section = self.options.load_gateway_section.clone();
        let opt_agent_dir = self.options.agent_dir.clone();
        let opt_create_store = self.options.create_store.clone();

        let ensure_store = Arc::new(move || -> Result<Option<Arc<dyn GatewayRulesStore>>, String> {
            let mut guard = store_holder.lock().map_err(|e| format!("Mutex poisoned: {e}"))?;
            if let Some(store) = &*guard {
                return Ok(Some(store.clone()));
            }
            if settled.load(Ordering::Relaxed) {
                return Ok(None);
            }

            let section = match &opt_load_section {
                Some(f) => f(),
                None => default_gateway_section(),
            };

            if !has_scopes(&section) {
                settled.store(true, Ordering::Relaxed);
                return Ok(None);
            }

            let agent_dir = match &opt_agent_dir {
                Some(f) => f(),
                None => default_agent_dir(),
            };

            let db_path = gateway_database_path(&agent_dir);
            if !db_path.exists() {
                settled.store(true, Ordering::Relaxed);
                return Ok(None);
            }

            let store = match &opt_create_store {
                Some(f) => f(&agent_dir)?,
                None => default_create_store(&agent_dir)?,
            };

            *guard = Some(store.clone());
            Ok(Some(store))
        });

        let prompt_handler = Arc::new(GatewayRulesPromptHandler::new(GatewayRulesPromptOptions {
            ensure_store,
            on_lookup_error: Some(Arc::new(move |msg: &str| {
                if !registration_warned.swap(true, Ordering::Relaxed) {
                    maho_ext_api::diagnostic(msg);
                }
            })),
        }));

        api.on(
            EventKind::BeforeAgentStart,
            Arc::new(move |event, ctx| {
                let handler = prompt_handler.clone();
                let session_id = ctx.session_manager.session_id().to_string();
                if let ExtensionEvent::BeforeAgentStart(payload) = event
                    && let Some(res) = handler.handle(&payload.system_prompt, &session_id)
                {
                    return Box::pin(async move {
                        Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult { system_prompt_parts: None,
                            message: None,
                            system_prompt: Some(res.system_prompt),
                        }))
                    });
                }
                Box::pin(async move { Ok(EventResult::None) })
            }),
        );
    }
}

/// Helper function to check if the gateway section contains a non-empty scopes array.
pub fn has_scopes(section: &Option<Value>) -> bool {
    match section {
        Some(Value::Object(map)) => match map.get("scopes") {
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

fn default_gateway_section() -> Option<Value> {
    let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let res = load_senpi_omo_config(LoadOmoConfigOptions {
        cwd,
        env: Some(env_map),
        harness: Some("senpi".into()),
        ..Default::default()
    });
    res.config.get("gateway").cloned()
}

fn default_agent_dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    resolve_agent_home(&env_map, &home, &cwd, |p| p.exists())
}

fn default_create_store(agent_dir: &std::path::Path) -> Result<Arc<dyn GatewayRulesStore>, String> {
    let db_path = gateway_database_path(agent_dir);
    let mut conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
    apply_gateway_rules_migrations(&mut conn).map_err(|e| e.to_string())?;
    Ok(Arc::new(SqliteGatewayRulesStore::new(conn)))
}
