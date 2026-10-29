//! The `<builtin:x-search>` extension: load gate (spec §1), tool registration (§2) and skill
//! contribution (§6).

use crate::call::{CallDeps, CredentialLookup, DEFAULT_DEADLINE, StoredCredential};
use crate::request::{HttpClient, ReqwestClient};
use crate::skill::{self, WarnSink};
use crate::{EnvLookup, SEARCH_GROUP, TOOL_NAME, availability, params};
use maho_ext_api::{
    AgentToolResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent,
    ModelRegistry, ResourceDiscoverEntry, ResourcesDiscoverResult, SourceScope, ToolDefinition,
    ToolError, ToolExecutionMode, ToolExposure,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// The provider id of xAI credentials in the host's store (interface fact).
pub const PROVIDER: &str = "xai";

/// Reads the `disabled_skills` denylist for a working directory.
pub type DisabledSkills = Arc<dyn Fn(&Path) -> Vec<String> + Send + Sync>;

/// The X search extension. Every seam has a production default ([`XSearchExtension::new`]).
#[derive(Clone)]
pub struct XSearchExtension {
    /// Agent directory holding `auth.json`.
    pub agent_dir: PathBuf,
    pub env: EnvLookup,
    pub http: Arc<dyn HttpClient>,
    pub deadline: Duration,
    /// The skill file to contribute; `None` uses the copy embedded in the binary.
    pub skill_file: Option<PathBuf>,
    pub disabled_skills: DisabledSkills,
    pub warn: WarnSink,
    warned_missing_skill: Arc<AtomicBool>,
}

impl XSearchExtension {
    /// The production extension for `agent_dir` (process env, reqwest, embedded skill,
    /// `omo.jsonc` denylist, stderr diagnostics).
    pub fn new(agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            env: crate::process_env_lookup(),
            http: Arc::new(ReqwestClient::default()),
            deadline: DEFAULT_DEADLINE,
            skill_file: None,
            disabled_skills: Arc::new(|cwd: &Path| {
                disabled_skills(cwd, &maho_config::process_env())
            }),
            warn: Arc::new(maho_ext_api::diagnostic),
            warned_missing_skill: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether this extension registers anything right now (spec §1, §7 offline skip).
    pub fn is_available(&self) -> bool {
        let env = self.env.as_ref();
        !availability::is_offline(env) && availability::is_connected(&self.agent_dir, env)
    }

    fn resolve_skill(&self) -> Option<PathBuf> {
        let path = match &self.skill_file {
            Some(path) => path.clone(),
            None => match skill::embedded_skill_path() {
                Some(path) => path,
                None => std::env::temp_dir().join("maho-x-search").join("SKILL.md"),
            },
        };
        skill::resolve_skill_file(&path, &self.warned_missing_skill, &self.warn)
    }

    /// The skill paths contributed from `resources_discover` for `cwd`.
    pub fn discovered_skill_paths(
        &self,
        skill: Option<&Path>,
        cwd: &Path,
    ) -> Vec<ResourceDiscoverEntry> {
        let Some(skill) = skill else {
            return Vec::new();
        };
        if (self.disabled_skills)(cwd)
            .iter()
            .any(|name| name == skill::SKILL_NAME)
        {
            return Vec::new();
        }
        vec![ResourceDiscoverEntry {
            path: skill.to_string_lossy().into_owned(),
            scope: Some(SourceScope::System),
        }]
    }
}

/// The `disabled_skills` union over the layered `omo.jsonc` files for `cwd`.
pub fn disabled_skills(cwd: &Path, env: &maho_config::Env) -> Vec<String> {
    let loaded = maho_config::load_native(cwd, env);
    maho_config::collect_disabled_skills(
        &loaded.layers,
        &maho_config::Harness::Native,
        loaded.profile.as_deref(),
    )
}

/// Credential lookup over the host's model registry: a stored `xai` credential resolves through
/// provider auth; no stored credential (or a registry without credential metadata) → absent.
pub fn registry_credentials(registry: Arc<dyn ModelRegistry>) -> CredentialLookup {
    Arc::new(move || {
        let registry = registry.clone();
        let future: crate::call::CredentialFuture = Box::pin(async move {
            match registry.get_stored_credential_type(PROVIDER) {
                Ok(Some(_)) => StoredCredential::Present(
                    registry
                        .get_provider_auth(PROVIDER)
                        .await
                        .map(|auth| auth.and_then(|auth| auth.auth.api_key))
                        .map_err(|error| error.message),
                ),
                Ok(None) | Err(_) => StoredCredential::Absent,
            }
        });
        future
    })
}

/// The tool definition (deferred, searchable, parallel).
pub fn definition() -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        TOOL_NAME,
        "Search recent posts on X (Twitter) through xAI's server-side X search and get back post URLs with short snippets. Needs a connected xAI credential (/login to xAI or XAI_API_KEY).",
        params::schema(),
        Arc::new(|_| {
            Box::pin(async { Err(ToolError::Message("Extension context required".into())) })
        }),
    );
    "X Search".clone_into(&mut definition.label);
    definition.exposure = Some(ToolExposure::Search);
    definition.search_group = Some(SEARCH_GROUP.into());
    definition.search_keywords = Some(
        [
            "x posts",
            "tweets",
            "twitter",
            "x.com",
            "search x",
            "social media posts",
            "grok x search",
            "what people say on x",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    definition.prompt_snippet =
        Some("Find posts on X (Twitter) by topic, account or date range".into());
    definition.execution_mode = Some(ToolExecutionMode::Parallel);
    definition
}

impl Extension for XSearchExtension {
    fn register(&self, api: &mut ExtensionApi) {
        if !self.is_available() {
            return;
        }
        let deps = CallDeps {
            http: self.http.clone(),
            env: self.env.clone(),
            deadline: self.deadline,
        };
        if let Err(error) = api.register_tool_with_extension_context(
            definition(),
            Arc::new(move |_id, input, signal, _update, ctx| {
                let deps = deps.clone();
                let lookup = registry_credentials(ctx.model_registry.clone());
                Box::pin(async move {
                    let output = crate::call::run(&input, &deps, &lookup, signal.as_ref()).await;
                    let mut result = AgentToolResult::text(output.text);
                    result.details = output.details;
                    if output.is_error {
                        result.is_error = Some(true);
                    }
                    Ok(result)
                })
            }),
        ) {
            (self.warn)(&format!("x-search: {error}"));
            return;
        }

        let skill = self.resolve_skill();
        let this = self.clone();
        api.on(
            EventKind::ResourcesDiscover,
            Arc::new(move |event, _ctx| {
                let skill_paths = match event {
                    ExtensionEvent::ResourcesDiscover(event) => {
                        this.discovered_skill_paths(skill.as_deref(), &event.cwd)
                    }
                    _ => Vec::new(),
                };
                Box::pin(async move {
                    Ok(if skill_paths.is_empty() {
                        EventResult::None
                    } else {
                        EventResult::ResourcesDiscover(ResourcesDiscoverResult {
                            skill_paths,
                            ..Default::default()
                        })
                    })
                })
            }),
        );
    }
}
