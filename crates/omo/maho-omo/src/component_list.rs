//! Port of `omo-senpi/src/extension/component-list.ts` at pin `77f3067f1`: the ordered component
//! registration list. Names and order are upstream's.
//!
//! Upstream `createOmoSenpiComponents(taskComponent)` always supplies both the `task` component
//! (its argument) and `memory` (`createMemoryComponent()`); the list has no optional entry. The
//! native `task` and `memory` factories are owned by todos 44 and 43/45, so this crate cannot build
//! them locally - but it must not silently drop them either, because `compose.ts` also registers a
//! disabled flag per list entry. [`omo_components`] therefore requires both, and
//! [`try_omo_components`] reports exactly which are missing instead of producing a short list.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::compose::OmoSenpiComponent;

/// Upstream `getBuiltinSkillsRoot()`: the packaged `skills/` directory beside the extension.
pub const SKILLS_ROOT_ENV: &str = "OMO_SENPI_SKILLS_ROOT";

pub fn builtin_skills_root() -> PathBuf {
    skills_root_from(std::env::var(SKILLS_ROOT_ENV).ok())
}

/// The `OMO_SENPI_SKILLS_ROOT` contract: a set, non-empty value wins; otherwise the packaged
/// `skills/` directory beside the executable (or a relative `skills` when the path is unknown).
fn skills_root_from(env_root: Option<String>) -> PathBuf {
    if let Some(root) = env_root
        && !root.is_empty()
    {
        return PathBuf::from(root);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("skills")))
        .unwrap_or_else(|| PathBuf::from("skills"))
}

#[derive(Clone)]
pub struct OmoComponentOptions {
    pub skills_root: PathBuf,
    pub state_dir: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl Default for OmoComponentOptions {
    fn default() -> Self {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let telemetry_env: std::collections::HashMap<String, String> = env.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
        Self {
            skills_root: builtin_skills_root(),
            state_dir: maho_omo_telemetry::product_identity::get_omo_native_state_dir(&telemetry_env),
            env,
        }
    }
}

impl OmoComponentOptions {
    pub fn with_skills_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.skills_root = root.into();
        self
    }

    pub fn with_state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.state_dir = dir.into();
        self
    }

    fn skills_root_string(&self) -> String {
        self.skills_root.to_string_lossy().into_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingComponents {
    pub names: Vec<&'static str>,
}

impl std::fmt::Display for MissingComponents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the omo component list requires {}", self.names.join(", "))
    }
}

impl std::error::Error for MissingComponents {}

/// Wraps lane-43's `MemoryExtension` as a re-registration-safe list entry.
///
/// `MemoryExtension::new` consumes its `MemoryExtensionOptions`, so a shared instance registers once
/// and no-ops afterwards; `ExtensionRunner::recreate` re-registers the same extension objects, so a
/// single shared instance would leave memory unregistered after any reload. `build` is called once
/// per register and must return fresh options over the caller's ONE shared `MemoryComponent` /
/// `MemoryWiring` (never a second store).
pub fn memory_component_from(
    build: impl Fn() -> maho_omo_memory::composition::MemoryExtensionOptions + Send + Sync + 'static,
) -> OmoSenpiComponent {
    OmoSenpiComponent::from_factory("memory", move || Box::new(maho_omo_memory::composition::MemoryExtension::new(build())))
}

fn component(name: &'static str, extension: impl maho_ext_api::Extension + 'static) -> OmoSenpiComponent {
    OmoSenpiComponent::new(name, Box::new(extension))
}

/// Upstream `createOmoSenpiComponents(taskComponent)`: always the full 17 entries in upstream order.
pub fn omo_components(
    task: OmoSenpiComponent,
    memory: OmoSenpiComponent,
    options: &OmoComponentOptions,
) -> Vec<OmoSenpiComponent> {
    let skills_root = options.skills_root.clone();
    let mut components = Vec::with_capacity(17);
    components.push(component("config-startup", maho_omo_config_startup::ConfigStartupComponent::default()));
    components.push(component("native-badge", maho_omo_native_badge::NativeBadgeComponent));
    components.push(component(
        "onboarding",
        maho_omo_onboarding::OnboardingComponent {
            state_dir: options.state_dir.clone(),
            skills_root: options.skills_root_string(),
        },
    ));
    components.push(component("init-deep-advisor", maho_omo_init_deep_advisor::index::InitDeepAdvisorComponent::new(skills_root.clone())));
    components.push(component(
        "telemetry",
        maho_omo_telemetry::omo_native_component::OmoNativeTelemetryComponent::with_default_config(
            maho_omo_telemetry::index::SenpiTelemetryOptions {
                env: Some(options.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect()),
                state_dir: Some(options.state_dir.clone()),
                ..Default::default()
            },
            skills_root.clone(),
        ),
    ));
    components.push(component("ultrawork", maho_omo_ultrawork::UltraworkComponent::default()));
    components.push(component("mass-ulw", maho_omo_mass_ulw::MassUlwComponent { skills_root: options.skills_root_string() }));
    components.push(component("start-work-continuation", maho_omo_start_work_continuation::StartWorkContinuationComponent::default()));
    // The loop's registration-time log has no context; bind the shared runtime logger at
    // construction (same pattern as config-watch below) so the production path logs too.
    let loop_env = options.env.clone();
    components.push(OmoSenpiComponent::from_context_register("ulw-loop", move |api, runtime| {
        use maho_ext_api::Extension;
        let mut component = maho_omo_ulw_loop::index::UlwLoopComponent::from_env(&loop_env);
        component.logger = Some(runtime.logger());
        component.register(api);
    }));
    components.push(component("todo-fanout-reminder", maho_omo_todo_fanout_reminder::TodoFanoutReminderComponent::default()));
    components.push(component("fallback-architect", maho_omo_fallback_architect::FallbackArchitectComponent::default()));
    components.push(component("comment-checker", maho_omo_comment_checker::CommentCheckerComponent::default()));
    components.push(component("ast-grep", maho_omo_ast_grep::AstGrepComponent::default()));
    components.push(component("lsp", maho_omo_lsp::LspComponent));
    components.push(task);
    components.push(memory);
    let watch = std::sync::Arc::new(std::sync::Mutex::new(maho_omo_config_watch::index::ConfigWatchComponent::default()));
    components.push(OmoSenpiComponent::from_context_register("config-watch", move |api, runtime| {
        use maho_ext_api::Extension;
        let mut watch = watch.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        watch.options.logger = Some(runtime.logger());
        watch.register(api);
    }));
    components
}

pub fn try_omo_components(
    task: Option<OmoSenpiComponent>,
    memory: Option<OmoSenpiComponent>,
    options: &OmoComponentOptions,
) -> Result<Vec<OmoSenpiComponent>, MissingComponents> {
    let mut missing = Vec::new();
    if task.is_none() {
        missing.push("task");
    }
    if memory.is_none() {
        missing.push("memory");
    }
    if !missing.is_empty() {
        return Err(MissingComponents { names: missing });
    }
    let (Some(task), Some(memory)) = (task, memory) else {
        return Err(MissingComponents { names: missing });
    };
    Ok(omo_components(task, memory, options))
}

pub fn omo_component_names() -> Vec<&'static str> {
    [
        "config-startup",
        "native-badge",
        "onboarding",
        "init-deep-advisor",
        "telemetry",
        "ultrawork",
        "mass-ulw",
        "start-work-continuation",
        "ulw-loop",
        "todo-fanout-reminder",
        "fallback-architect",
        "comment-checker",
        "ast-grep",
        "lsp",
        "task",
        "memory",
        "config-watch",
    ]
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_non_empty_env_root_overrides_the_packaged_skills_dir() {
        assert_eq!(skills_root_from(Some("/custom/omo/skills".to_owned())), PathBuf::from("/custom/omo/skills"));
    }

    #[test]
    fn an_unset_or_empty_env_root_falls_back_to_the_packaged_skills_dir() {
        let fallback = skills_root_from(None);
        assert!(fallback.ends_with("skills"), "packaged fallback stays under a skills dir: {fallback:?}");
        assert_eq!(skills_root_from(Some(String::new())), fallback, "an empty override is treated as unset");
    }
}
