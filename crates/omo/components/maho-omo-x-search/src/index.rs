//! Port of omo-senpi `components/x-search/index.ts` at latest `455dee62`.
//!
//! Synchronous credential-gated extension-load tool registration,
//! conditional skill contribution, and user-skill priority.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::{
    ComponentLogger, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent, ResourceDiscoverEntry, ResourcesDiscoverResult, SourceScope,
};
use maho_omo_agent_home::resolve_agent_home;
use maho_omo_bundled_skills::contributed_skill::{
    host_commands_from_runtime, read_discover_cwd, resolve_contributed_skill, ContributedSkill,
    ResolveContributedSkillOptions,
};

use crate::auth::has_xai_credential;
use crate::client::XSearchFetch;
use crate::tool::{create_x_search_tool, CreateXSearchToolOptions};

pub use crate::tool::{X_SEARCH_MODEL_ENV, X_SEARCH_TOOL_DESCRIPTION, X_SEARCH_TOOL_NAME};

pub const X_SEARCH_COMPONENT_NAME: &str = "x-search";

pub type SkillPathResolver = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

#[derive(Clone, Default)]
pub struct XSearchComponentOptions {
    /// Overrides agent-home resolution; tests point it at a temp dir holding auth.json.
    pub agent_dir: Option<PathBuf>,
    pub env: Option<BTreeMap<String, String>>,
    pub home_dir: Option<PathBuf>,
    pub fetch_impl: Option<XSearchFetch>,
    pub resolve_skill_path: Option<SkillPathResolver>,
    pub logger: Option<Arc<dyn ComponentLogger>>,
}

#[derive(Clone, Default)]
pub struct XSearchComponent {
    pub options: XSearchComponentOptions,
}

impl XSearchComponent {
    pub const NAME: &'static str = X_SEARCH_COMPONENT_NAME;

    pub fn new(options: XSearchComponentOptions) -> Self {
        Self { options }
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Self {
        Self {
            options: XSearchComponentOptions {
                env: Some(env.clone()),
                ..Default::default()
            },
        }
    }
}

pub fn create_x_search_component(options: XSearchComponentOptions) -> XSearchComponent {
    XSearchComponent::new(options)
}

impl Extension for XSearchComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let env = self.options.env.clone().unwrap_or_else(|| std::env::vars().collect());
        let agent_dir = self.options.agent_dir.clone().unwrap_or_else(|| {
            let home = self
                .options
                .home_dir
                .clone()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from("/"));
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            resolve_agent_home(&env, &home, &cwd, Path::exists)
        });

        let connected = has_xai_credential(&agent_dir, &env);

        // Upstream logs:
        // if (!connected) { ctx.logger.debug?.("x-search skipped: no xAI credential", { component: X_SEARCH_COMPONENT_NAME }); return; }
        // ctx.logger.debug?.("x-search registered", { component: X_SEARCH_COMPONENT_NAME });
        // Since native ComponentLogger has no debug method (only info, warn, error),
        // and upstream explains: "Both outcomes are expected states: the default logger prints info to the
        // terminal before the TUI takes over, so they stay on the optional debug channel instead of
        // greeting every startup."
        // We stay silent here rather than printing noisy info notices.
        if !connected {
            return;
        }

        let (definition, executor) = create_x_search_tool(CreateXSearchToolOptions {
            fetch_impl: self.options.fetch_impl.clone(),
            env: Some(env.clone()),
            resolve_bearer: None,
        });

        if let Err(error) = api.register_tool_with_extension_context(definition, executor) {
            std::panic::panic_any(error);
        }

        let skill_resolver: SkillPathResolver = self
            .options
            .resolve_skill_path
            .clone()
            .unwrap_or_else(|| Arc::new(|| resolve_x_search_skill_path(None)));

        let skill_path = skill_resolver();
        let Some(skill_path) = skill_path else {
            if let Some(ref logger) = self.options.logger {
                logger.warn(
                    "x-search skill missing: x_search registered without its conditional skill",
                    Some(&serde_json::json!({ "component": X_SEARCH_COMPONENT_NAME })),
                );
            }
            return;
        };

        let runtime = api.runtime.clone();
        let env_disc = env.clone();

        api.on(
            EventKind::ResourcesDiscover,
            Arc::new(move |event, ctx| {
                let ExtensionEvent::ResourcesDiscover(discover) = event else {
                    return Box::pin(async { Ok(EventResult::None) });
                };

                let cwd = read_discover_cwd(discover).unwrap_or_else(|| ctx.cwd.clone());
                let host_cmds = host_commands_from_runtime(runtime.clone());
                let cmds = host_cmds().ok().flatten();

                let skill = resolve_contributed_skill(ResolveContributedSkillOptions {
                    commands: cmds.as_deref(),
                    name: X_SEARCH_COMPONENT_NAME,
                    path: skill_path.clone(),
                    cwd,
                    env: &env_disc,
                });

                let mut res = ResourcesDiscoverResult {
                    skill_paths: Vec::new(),
                    prompt_paths: Vec::new(),
                    theme_paths: Vec::new(),
                    hook_paths: Vec::new(),
                };

                if let ContributedSkill::Contributed { ref path } = skill {
                    res.skill_paths.push(ResourceDiscoverEntry {
                        path: path.to_string_lossy().to_string(),
                        scope: Some(SourceScope::System),
                    });
                }

                Box::pin(async move {
                    if res.skill_paths.is_empty() {
                        Ok(EventResult::None)
                    } else {
                        Ok(EventResult::ResourcesDiscover(res))
                    }
                })
            }),
        );
    }
}

pub fn resolve_x_search_skill_path(importer_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(base) = importer_path {
        let packaged = base.join("../skills-conditional/x-search/SKILL.md");
        if packaged.exists() {
            return std::fs::canonicalize(&packaged).ok().or(Some(packaged));
        }
        let local = base.join("./skill/SKILL.md");
        if local.exists() {
            return std::fs::canonicalize(&local).ok().or(Some(local));
        }
        return None;
    }

    let mut candidates = Vec::new();

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("skills-conditional/x-search/SKILL.md"));
        candidates.push(dir.join("plugin/skills-conditional/x-search/SKILL.md"));
        candidates.push(dir.join("skill/SKILL.md"));
    }

    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest_dir).join("skill/SKILL.md"));
    }

    candidates.into_iter().find(|p| p.exists())
}
