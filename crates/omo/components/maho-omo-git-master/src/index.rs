//! Port of omo-senpi `components/git-master/index.ts` at omo `455dee62`: the
//! `git-master-attribution` component. It registers ONE `tool_result` handler that appends the
//! commit-footer directive after a successful read of the `/git-master/SKILL.md` skill, using the
//! `git_master` settings resolved from the effective omo config for the session cwd.

use std::path::Path;
use std::sync::Arc;

use maho_ext_api::{
    ComponentLogger, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, JsonValue,
    ToolContent, ToolResultEventResult,
};
use maho_omo_config_resolution::load_senpi_omo_config;
use omo_config_core::{LoadOmoConfigOptions, OmoConfigEnv, resolve_omo_git_master_settings};

pub use crate::directive::{GitMasterCommitFooter, GitMasterSettings};
use crate::directive::build_git_master_attribution_directive;

/// Upstream `GIT_MASTER_SKILL_PATH_SUFFIX`: only a read of the git-master skill is attributed.
pub const GIT_MASTER_SKILL_PATH_SUFFIX: &str = "/git-master/SKILL.md";

/// Upstream `GitMasterAttributionComponentOptions.loadSettings`.
pub type LoadSettings = Arc<dyn Fn(&Path) -> GitMasterSettings + Send + Sync>;

/// Upstream `GitMasterAttributionComponentOptions`.
///
/// `env` is the native injection seam for the default loader (the same pattern as
/// `SenpiTelemetryOptions.env` and `ConfigStartupComponent::load_config`): production leaves it
/// `None`, which makes `load_senpi_omo_config` read the process environment exactly as upstream's
/// `loadSenpiOmoConfig({ cwd })` does; tests inject a temp `HOME` so the resolution is
/// deterministic.
#[derive(Clone, Default)]
pub struct GitMasterAttributionComponentOptions {
    pub load_settings: Option<LoadSettings>,
    pub env: Option<OmoConfigEnv>,
}

/// Upstream `defaultLoadSettings`: `resolveOmoGitMasterSettings(loadSenpiOmoConfig({ cwd }).config)`.
pub fn default_load_settings(cwd: &Path, env: Option<&OmoConfigEnv>) -> GitMasterSettings {
    let loaded = load_senpi_omo_config(LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: env.cloned(),
        ..Default::default()
    });
    GitMasterSettings::from_resolved(&resolve_omo_git_master_settings(&loaded.config))
}

/// Upstream `createGitMasterAttributionComponent`.
#[derive(Clone, Default)]
pub struct GitMasterAttributionComponent {
    pub options: GitMasterAttributionComponentOptions,
}

impl GitMasterAttributionComponent {
    pub fn new(options: GitMasterAttributionComponentOptions) -> Self {
        Self { options }
    }

    /// Registers with an explicit settings loader; the component's own default is
    /// [`default_load_settings`] over [`GitMasterAttributionComponentOptions::env`].
    pub fn with_load_settings(load_settings: LoadSettings) -> Self {
        Self::new(GitMasterAttributionComponentOptions {
            load_settings: Some(load_settings),
            env: None,
        })
    }
}

/// Upstream `readFilePath`: `input.file_path ?? input.path`, accepted only when it is a string.
fn read_file_path(input: &JsonValue) -> Option<&str> {
    input
        .get("file_path")
        .filter(|value| !value.is_null())
        .or_else(|| input.get("path").filter(|value| !value.is_null()))
        .and_then(JsonValue::as_str)
}

impl Extension for GitMasterAttributionComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let env = self.options.env.clone();
        let load_settings: LoadSettings = self.options.load_settings.clone().unwrap_or_else(move || {
            Arc::new(move |cwd: &Path| default_load_settings(cwd, env.as_ref()))
        });

        api.on(
            EventKind::ToolResult,
            Arc::new(move |event, ctx| {
                let load_settings = Arc::clone(&load_settings);
                Box::pin(async move {
                    let ExtensionEvent::ToolResult(event) = event else {
                        return Ok(EventResult::None);
                    };
                    // Upstream `asGitMasterReadResultEvent`: a successful `read` whose normalized
                    // path ends with the git-master skill suffix. Every other tool, error, and
                    // file is left untouched.
                    if event.tool_name != "read" || event.is_error {
                        return Ok(EventResult::None);
                    }
                    let Some(file_path) = read_file_path(&event.input) else {
                        return Ok(EventResult::None);
                    };
                    if !file_path.replace('\\', "/").ends_with(GIT_MASTER_SKILL_PATH_SUFFIX) {
                        return Ok(EventResult::None);
                    }

                    let settings = load_settings(&ctx.cwd);
                    let Some(directive) = build_git_master_attribution_directive(&settings) else {
                        return Ok(EventResult::None);
                    };
                    log_appended(ctx.logger.as_ref(), &settings.commit_footer);

                    let mut content = event.content.clone();
                    content.push(ToolContent::text(directive));
                    Ok(EventResult::ToolResult(ToolResultEventResult {
                        content: Some(content),
                        ..Default::default()
                    }))
                })
            }),
        );
    }
}

/// Upstream `ctx.logger.info("omo-senpi git-master attribution appended", { commitFooter })`.
fn log_appended(logger: Option<&Arc<dyn ComponentLogger>>, footer: &GitMasterCommitFooter) {
    let Some(logger) = logger else {
        return;
    };
    let commit_footer = match footer {
        GitMasterCommitFooter::Disabled => JsonValue::Bool(false),
        GitMasterCommitFooter::Builtin => JsonValue::Bool(true),
        GitMasterCommitFooter::Custom(text) => JsonValue::String(text.clone()),
    };
    let details =
        JsonValue::Object([("commitFooter".to_owned(), commit_footer)].into_iter().collect());
    logger.info("omo-senpi git-master attribution appended", Some(&details));
}
