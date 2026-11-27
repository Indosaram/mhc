use std::collections::HashMap;
use std::sync::Arc;

pub const SESSION_ENVIRONMENT_KEYS: [&str; 8] = [
    "PI_SESSION_ID",
    "PI_SESSION_FILE",
    "PI_SESSION_CWD",
    "PI_GOAL_STORE_FILE",
    "PI_PROVIDER",
    "PI_MODEL",
    "PI_REASONING_LEVEL",
    "OMO_BROWSER_ENGINE",
];

/// The environment a kernel and its children inherit from the host (`senpi#5635`): `process.env`
/// without the RPC host's supervisor hand-off, computed at each spawn.
pub type InheritedEnvironment = Arc<dyn Fn() -> HashMap<String, String> + Send + Sync>;

/// How `inherited` differs from `base`: the names it drops (`removedEnv`) and the values it sets
/// (`inheritedSet`). Worker-thread JS kernels get these instead of a fresh env because their
/// `process.env` starts as a copy of the host's.
pub fn inherited_environment_delta(
    base: &HashMap<String, String>,
    inherited: &HashMap<String, String>,
) -> (Vec<String>, HashMap<String, String>) {
    let removed_env = base
        .keys()
        .filter(|name| !inherited.contains_key(name.as_str()))
        .cloned()
        .collect();
    let mut inherited_set = HashMap::new();
    for (name, value) in inherited {
        if base.get(name) != Some(value) {
            inherited_set.insert(name.clone(), value.clone());
        }
    }
    (removed_env, inherited_set)
}

pub type SessionEnvironment = HashMap<String, String>;

pub struct SessionEnvironmentSource<'a> {
    pub cwd: &'a str,
    pub goal_store_file: Option<&'a str>,
    pub session_id: &'a str,
    pub session_file: Option<&'a str>,
    pub model: Option<(&'a str, &'a str)>,
    pub thinking_level: Option<&'a str>,
    pub browser_engine: Option<&'a str>,
}

pub fn session_environment_from(source: &SessionEnvironmentSource<'_>) -> SessionEnvironment {
    let mut env = SessionEnvironment::from([
        ("PI_SESSION_ID".into(), source.session_id.into()),
        ("PI_SESSION_CWD".into(), source.cwd.into()),
    ]);
    for (key, value) in [
        ("PI_GOAL_STORE_FILE", source.goal_store_file),
        ("PI_SESSION_FILE", source.session_file),
        ("PI_REASONING_LEVEL", source.thinking_level),
        ("OMO_BROWSER_ENGINE", source.browser_engine),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            env.insert(key.into(), value.into());
        }
    }
    if let Some((provider, model)) = source.model {
        env.insert("PI_PROVIDER".into(), provider.into());
        env.insert("PI_MODEL".into(), model.into());
    }
    env
}

pub fn apply_session_environment(
    base: &SessionEnvironment,
    session: Option<&SessionEnvironment>,
) -> SessionEnvironment {
    let mut merged = base.clone();
    for key in SESSION_ENVIRONMENT_KEYS {
        merged.remove(key);
    }
    if let Some(session) = session {
        merged.extend(session.clone());
    }
    merged
}

pub fn session_environment_from_context(
    context: &dyn maho_ext_api::ToolContext,
) -> SessionEnvironment {
    let cwd = context.cwd().to_string_lossy();
    let goal = context.goal_store_file().map(|path| path.to_string_lossy());
    let session = context.session_manager();
    let file = session.session_file().map(|path| path.to_string_lossy());
    let thinking = context.thinking_level().map(|level| match level {
        maho_ext_api::ThinkingLevel::Minimal => "minimal",
        maho_ext_api::ThinkingLevel::Low => "low",
        maho_ext_api::ThinkingLevel::Medium => "medium",
        maho_ext_api::ThinkingLevel::High => "high",
        maho_ext_api::ThinkingLevel::Xhigh => "xhigh",
        maho_ext_api::ThinkingLevel::Max => "max",
    });
    session_environment_from(&SessionEnvironmentSource {
        cwd: &cwd,
        goal_store_file: goal.as_deref(),
        session_id: session.session_id(),
        session_file: file.as_deref(),
        model: context
            .model()
            .map(|model| (model.provider.as_str(), model.id.as_str())),
        thinking_level: thinking,
        browser_engine: context.browser_engine(),
    })
}
