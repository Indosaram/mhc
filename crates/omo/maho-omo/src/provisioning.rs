//! Ports of `omo-senpi/src/extension/{toolkit-path-provisioning,dag-sdk-root-provisioning}.ts`.
//!
//! Both run at activation before any component registers, and neither may throw. Upstream mutates
//! `process.env`; `std::env::set_var` is unsafe under edition 2024 and this crate forbids unsafe
//! code, so each provisioner returns the environment updates and hands them to an injectable
//! [`EnvWriter`] sink. `OmoExtension::environment` retains updates for explicit child mounting.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const TOOLKIT_BIN_ENV: &str = "OMO_AGENT_TOOLKIT_BIN";
pub const DAG_SDK_ROOT_ENV: &str = "OMO_DAG_SDK_ROOT";

pub type EnvReader = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type EnvWriter = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;

pub fn path_delimiter() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

pub fn toolkit_base_dir_default() -> PathBuf {
    runtime_dir("agent-toolkit")
}

pub fn dag_sdk_base_dir_default() -> PathBuf {
    runtime_dir("dag")
}

fn runtime_dir(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("runtime").join(name)))
        .unwrap_or_else(|| PathBuf::from("runtime").join(name))
}

#[derive(Clone)]
pub struct ProvisioningOptions {
    pub toolkit_base_dir: Option<PathBuf>,
    pub dag_sdk_base_dir: Option<PathBuf>,
    pub env: Option<EnvReader>,
    pub sink: Option<EnvWriter>,
}

impl Default for ProvisioningOptions {
    fn default() -> Self {
        Self { toolkit_base_dir: None, dag_sdk_base_dir: None, env: None, sink: None }
    }
}

impl ProvisioningOptions {
    fn read(&self, name: &str) -> Option<String> {
        match &self.env {
            Some(reader) => reader(name),
            None => std::env::var(name).ok(),
        }
    }

    fn apply(&self, updates: Vec<(String, Option<String>)>) {
        if let Some(sink) = &self.sink {
            for (name, value) in updates {
                sink(&name, value.as_deref());
            }
        }
    }
}

/// Upstream `createToolkitPathProvisioning`: prepend the toolkit dir to `PATH` when present and
/// set `OMO_AGENT_TOOLKIT_BIN` when unset. Returns the updates it would apply.
pub fn provision_toolkit_path(options: &ProvisioningOptions) -> Vec<(String, Option<String>)> {
    let base_dir = options.toolkit_base_dir.clone().unwrap_or_else(toolkit_base_dir_default);
    let mut updates = Vec::new();
    if !base_dir.is_dir() {
        return updates;
    }
    let base = base_dir.to_string_lossy().into_owned();
    let current = options.read("PATH").unwrap_or_default();
    let first = current.split(path_delimiter()).next().unwrap_or_default();
    if first != base {
        let next = if current.is_empty() { base.clone() } else { format!("{base}{}{current}", path_delimiter()) };
        updates.push(("PATH".to_owned(), Some(next)));
    }
    let toolkit_bin = options.read(TOOLKIT_BIN_ENV);
    if toolkit_bin.as_deref().is_none_or(str::is_empty) {
        updates.push((TOOLKIT_BIN_ENV.to_owned(), Some(base_dir.join("cli.js").to_string_lossy().into_owned())));
    }
    options.apply(updates.clone());
    updates
}

/// Upstream `createDagSdkRootProvisioning`: publish the dag sdk directory when it exists.
pub fn provision_dag_sdk_root(options: &ProvisioningOptions) -> Vec<(String, Option<String>)> {
    let base_dir = options.dag_sdk_base_dir.clone().unwrap_or_else(dag_sdk_base_dir_default);
    if !base_dir.is_dir() {
        return Vec::new();
    }
    let updates = vec![(DAG_SDK_ROOT_ENV.to_owned(), Some(base_dir.to_string_lossy().into_owned()))];
    options.apply(updates.clone());
    updates
}

/// True when the first `PATH` entry already is `base_dir`.
pub fn path_already_prepended(path: &str, base_dir: &Path) -> bool {
    path.split(path_delimiter()).next() == Some(base_dir.to_string_lossy().as_ref())
}
