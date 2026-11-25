use crate::{ExtensionRuntime, RegisteredMcpServer, RegisteredMcpServerDeclaration};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

pub type McpChangeListener = Arc<dyn Fn(ExtensionRuntime, Vec<RegisteredMcpServer>) + Send + Sync>;
pub type WeakMcpChangeListener =
    Weak<dyn Fn(ExtensionRuntime, Vec<RegisteredMcpServer>) + Send + Sync>;

/// The MCP servers extensions registered with `register_mcp_server()`, read from the extension
/// records themselves (senpi `mcp-server-registry.ts`, #4604).
///
/// Until a runner binds, every tracked extension counts in load order; once bound, only the runner's
/// final extension set counts in runner order, so an extension dropped after loading registers nothing.
#[derive(Default)]
pub struct McpServerRegistry {
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    loaded: Vec<McpRegistrar>,
    bound: Option<Vec<McpRegistrar>>,
    warned_clashes: BTreeSet<(String, String, String)>,
    change_listener: Option<WeakMcpChangeListener>,
}

#[derive(Clone)]
pub struct McpRegistrar {
    pub path: String,
    pub declarations: Vec<RegisteredMcpServerDeclaration>,
}

impl McpServerRegistry {
    pub fn track(&self, path: &str, declarations: Vec<RegisteredMcpServerDeclaration>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = state.loaded.iter_mut().find(|entry| entry.path == path) {
            existing.declarations = declarations;
        } else {
            state.loaded.push(McpRegistrar {
                path: path.to_owned(),
                declarations,
            });
        }
    }

    pub fn untrack(&self, path: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.loaded.retain(|entry| entry.path != path);
    }

    pub fn update_extension(&self, path: &str, declarations: Vec<RegisteredMcpServerDeclaration>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bound) = state.bound.as_mut()
            && let Some(entry) = bound.iter_mut().find(|entry| entry.path == path)
        {
            entry.declarations = declarations.clone();
        }
        if let Some(entry) = state.loaded.iter_mut().find(|entry| entry.path == path) {
            entry.declarations = declarations;
        } else {
            state.loaded.push(McpRegistrar {
                path: path.to_owned(),
                declarations,
            });
        }
    }

    /// The runner's final extension set; registrations of any other extension are no longer visible.
    pub fn bind(&self, extensions: Vec<McpRegistrar>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.bound = Some(extensions);
    }

    pub fn set_change_listener(&self, listener: Option<WeakMcpChangeListener>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.change_listener = listener;
    }

    /// Notify the listener (set by the runner) that an extension registered or unregistered a server.
    pub fn changed(&self, runtime: &ExtensionRuntime) {
        let (listener, servers) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let listener = state.change_listener.as_ref().and_then(Weak::upgrade);
            let servers = listener.as_ref().map(|_| collect_list(&mut state));
            (listener, servers)
        };
        if let (Some(listener), Some(servers)) = (listener, servers) {
            listener(runtime.clone(), servers);
        }
    }

    /// First declaration per name wins, in extension order.
    pub fn declarations(&self) -> Vec<RegisteredMcpServerDeclaration> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        collect_declarations(&mut state)
    }

    /// Copies in upstream's `RegisteredMcpServer` shape, for extensions.
    pub fn list(&self) -> Vec<RegisteredMcpServer> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        collect_list(&mut state)
    }
}

fn collect_declarations(state: &mut RegistryState) -> Vec<RegisteredMcpServerDeclaration> {
    let mut by_name: BTreeMap<String, RegisteredMcpServerDeclaration> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut clashes: Vec<(String, String, String)> = Vec::new();
    let entries = state.bound.as_deref().unwrap_or(&state.loaded);
    for extension in entries {
        for declaration in &extension.declarations {
            if let Some(existing) = by_name.get(&declaration.name) {
                clashes.push((
                    existing.name.clone(),
                    existing.extension_path.clone(),
                    declaration.extension_path.clone(),
                ));
            } else {
                order.push(declaration.name.clone());
                by_name.insert(declaration.name.clone(), declaration.clone());
            }
        }
    }
    for key in clashes {
        if state.warned_clashes.insert(key.clone()) {
            crate::diagnostic(&format!(
                "MCP server '{}' declared by both {} and {}; keeping first declaration from {}.",
                key.0, key.1, key.2, key.1
            ));
        }
    }
    order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect()
}

fn collect_list(state: &mut RegistryState) -> Vec<RegisteredMcpServer> {
    collect_declarations(state)
        .into_iter()
        .map(|declaration| declaration.to_public())
        .collect()
}
