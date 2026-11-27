//! Port of senpi `packages/coding-agent/src/core/trust-requiring-resources.ts`.

pub use crate::trust_manager::{
    TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES, has_trust_requiring_project_resources,
    project_agents_skill_dirs,
};

pub async fn has_trust_requiring_project_resources_async(
    cwd: &str,
    user_home: Option<&str>,
) -> bool {
    let cwd = cwd.to_owned();
    let user_home = user_home.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        has_trust_requiring_project_resources(&cwd, user_home.as_deref())
    })
    .await
    .unwrap_or(false)
}
