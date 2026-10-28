//! The `tool_result` handler on the maho extension API (spec §1, §6).
//!
//! Register this extension before the LSP extension: handlers run in registration order, so the
//! diagnostics hook then sees the formatted text.
use crate::engine::{FormatEngine, FormatRequest};
use crate::lsp::{LspFormat, LspPoolFormatter, NoLsp};
use crate::run::Host;
use crate::settings::Settings;
use crate::MUTATION_TOOLS;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ToolContent,
    ToolResultEventResult,
};
use maho_lsp::config::ConfigPaths;
use maho_lsp::manager::LspManagerOptions;
use maho_lsp::tools::LspTools;
use std::path::PathBuf;
use std::sync::Arc;

/// Format-after-edit extension.
#[derive(Clone, Debug, Default)]
pub struct FormatExtension {
    /// Agent dir holding `lsp-client.json` (for the LSP formatting path).
    pub agent_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// Ask a running language server to format first (spec §5 step 1).
    pub use_lsp: bool,
    /// Fixed settings; `None` reads `formatOnMutation` from the session cwd's config.
    pub settings: Option<Settings>,
}

impl FormatExtension {
    /// The production setup: LSP path on, settings from config.
    pub fn from_env(agent_dir: Option<PathBuf>) -> Self {
        let home =
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        Self {
            agent_dir,
            home,
            use_lsp: true,
            settings: None,
        }
    }
}

impl Extension for FormatExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let lsp: Arc<dyn LspFormat> = if self.use_lsp {
            let config =
                ConfigPaths::standard(&api.cwd, self.agent_dir.as_deref(), self.home.as_deref());
            let manager = maho_lsp::extension::shared_manager(&LspManagerOptions::default());
            Arc::new(LspPoolFormatter {
                tools: LspTools::new(manager, config),
            })
        } else {
            Arc::new(NoLsp)
        };
        let engine = Arc::new(FormatEngine::new(Arc::new(Host::default()), lsp));
        let settings = self.settings.clone();
        api.on(
            EventKind::ToolResult,
            Arc::new(move |event, ctx| {
                let engine = Arc::clone(&engine);
                let settings = settings.clone();
                Box::pin(async move {
                    let ExtensionEvent::ToolResult(event) = event else {
                        return Ok(EventResult::None);
                    };
                    if event.is_error || !MUTATION_TOOLS.contains(&event.tool_name.as_str()) {
                        return Ok(EventResult::None);
                    }
                    let request = FormatRequest {
                        tool_name: event.tool_name.clone(),
                        input: event.input.clone(),
                        details: event.details.clone(),
                        cwd: ctx.cwd.clone(),
                        session_id: ctx.session_manager.session_id().to_owned(),
                        settings,
                    };
                    let report = engine.process(&request).await;
                    for warning in &report.warnings {
                        match &ctx.logger {
                            Some(logger) => logger.warn(warning, None),
                            None => maho_ext_api::diagnostic(warning),
                        }
                    }
                    if report.blocks.is_empty() && !report.is_error {
                        return Ok(EventResult::None);
                    }
                    let mut content = event.content.clone();
                    content.extend(report.blocks.into_iter().map(ToolContent::text));
                    Ok(EventResult::ToolResult(ToolResultEventResult {
                        content: Some(content),
                        details: None,
                        is_error: report.is_error.then_some(true),
                        usage: None,
                    }))
                })
            }),
        );
    }
}
