use std::collections::HashMap;

pub const SESSION_ENVIRONMENT_KEYS: [&str; 7] = [
    "PI_SESSION_ID", "PI_SESSION_FILE", "PI_SESSION_CWD", "PI_GOAL_STORE_FILE",
    "PI_PROVIDER", "PI_MODEL", "PI_REASONING_LEVEL",
];

pub type SessionEnvironment = HashMap<String, String>;

pub struct SessionEnvironmentSource<'a> {
    pub cwd: &'a str,
    pub goal_store_file: Option<&'a str>,
    pub session_id: &'a str,
    pub session_file: Option<&'a str>,
    pub model: Option<(&'a str, &'a str)>,
    pub thinking_level: Option<&'a str>,
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
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) { env.insert(key.into(), value.into()); }
    }
    if let Some((provider, model)) = source.model {
        env.insert("PI_PROVIDER".into(), provider.into());
        env.insert("PI_MODEL".into(), model.into());
    }
    env
}

pub fn apply_session_environment(base: &SessionEnvironment, session: Option<&SessionEnvironment>) -> SessionEnvironment {
    let mut merged = base.clone();
    for key in SESSION_ENVIRONMENT_KEYS { merged.remove(key); }
    if let Some(session) = session { merged.extend(session.clone()); }
    merged
}
