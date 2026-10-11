use std::path::{Path, PathBuf, Component};
use serde_json::{Value, json};
use maho_tools::definition::KernelPrelude;
use crate::bridge::{protocol::BridgeConnectionConfig, reserved::*};
use crate::kernels::shared::kernel_prelude_plan::{KernelPreludePlan, KernelPreludeTracker};
use super::rewrite_imports::rewrite_imports;

pub const PREPARED_CELL_PREFIX: &str = "/*senpi:prepared-cell*/";

pub struct LocalModuleLoaderOptions {
    pub cwd: PathBuf,
    pub local_roots: Option<crate::bridge::protocol::LocalRoots>,
    pub artifacts_dir: Option<PathBuf>,
}

pub fn local_bridge_connection(options: &LocalModuleLoaderOptions) -> BridgeConnectionConfig {
    BridgeConnectionConfig { port:1, token:"local".into(), local_roots:options.local_roots.clone(), artifacts_dir:options.artifacts_dir.as_ref().map(|path|path.to_string_lossy().into_owned()), parallel_pool_width:None }
}

fn directory_url(directory: &Path) -> Result<String, std::io::Error> {
    let absolute = if directory.is_absolute() { directory.to_owned() } else { std::env::current_dir()?.join(directory) };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component { Component::CurDir => {}, Component::ParentDir => { normalized.pop(); }, _ => normalized.push(component.as_os_str()) }
    }
    url::Url::from_directory_path(normalized).map(|url|url.to_string()).map_err(|()|std::io::Error::new(std::io::ErrorKind::InvalidInput, "module directory URL is invalid"))
}

pub fn runtime_context(options: &LocalModuleLoaderOptions) -> Result<Value, std::io::Error> {
    let mut roots = serde_json::Map::new();
    if let Some(local_roots) = &options.local_roots {
        for (scheme, root) in &local_roots.0 { roots.insert(scheme.to_lowercase(), json!(directory_url(Path::new(root))?)); }
    }
    if let Some(artifacts) = &options.artifacts_dir && !roots.contains_key("local") { roots.insert("local".into(), json!(directory_url(&artifacts.join("local"))?)); }
    Ok(json!({"cwdUrl":directory_url(&options.cwd)?, "localRootUrls":roots,
        "reservedAgentTool":RESERVED_AGENT_TOOL, "reservedOutputTool":RESERVED_OUTPUT_TOOL,
        "reservedSchemaTool":RESERVED_SCHEMA_TOOL,
        "timeoutPauseOp":TIMEOUT_PAUSE_OP, "timeoutResumeOp":TIMEOUT_RESUME_OP}))
}

pub fn contribution_prelude(plan: &KernelPreludePlan) -> String {
    let mut parts = Vec::new();
    for name in &plan.remove {
        parts.push(format!("delete globalThis[{}];", json!(name)));
    }
    for contribution in &plan.install {
        let missing = contribution
            .exports
            .iter()
            .map(|name| format!("!globalThis[{}]", json!(name)))
            .collect::<Vec<_>>()
            .join(" || ");
        if !missing.is_empty() {
            parts.push(format!("if ({missing}) {{\n{}\n}}", contribution.javascript));
        } else if !contribution.javascript.is_empty() {
            parts.push(contribution.javascript.clone());
        }
    }
    parts.join("\n")
}

pub struct LocalModuleLoader {
    prelude: String,
    tracker: std::sync::Mutex<KernelPreludeTracker>,
}

impl LocalModuleLoader {
    pub fn new(options: &LocalModuleLoaderOptions) -> Result<Self, std::io::Error> {
        let context = runtime_context(options)?;
        let prelude = format!("globalThis.__senpi_module_context__ = {context};\n{}", include_str!("local-module-loader.js").trim_end());
        Ok(Self {
            prelude,
            tracker: std::sync::Mutex::new(KernelPreludeTracker::default()),
        })
    }

    pub fn prepare_cell(&self, code: &str, preludes: &[KernelPrelude]) -> String {
        let plan = self
            .tracker
            .lock()
            .expect("prelude tracker lock")
            .plan(preludes);
        let extra = contribution_prelude(&plan);
        let full_prelude = if extra.is_empty() {
            self.prelude.clone()
        } else {
            format!("{}\n{}", self.prelude, extra)
        };
        format!("{PREPARED_CELL_PREFIX}{}", json!({"prelude":full_prelude,"code":rewrite_imports(code)}))
    }
}
