//! Port of senpi packages/ai/src/api/google-vertex-env.ts.
//!
//! `buildGoogleAuthOptions` already lives in `api/google_vertex.rs` (owned by the Vertex wire
//! module); it is re-exported here so callers reach both helpers through the upstream module.

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;

pub use super::google_vertex::{GoogleAuthOptions, build_google_auth_options};

pub fn resolve_vertex_project_from_env(
    project: Option<&str>,
    env: Option<&ProviderEnv>,
) -> Option<String> {
    project
        .filter(|project| !project.is_empty())
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_PROJECT", env))
        .or_else(|| get_provider_env_value("GCLOUD_PROJECT", env))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_explicit_project_then_scoped_env_names_in_order() {
        let mut env = ProviderEnv::new();
        env.insert("GCLOUD_PROJECT".into(), "gcloud".into());
        assert_eq!(
            resolve_vertex_project_from_env(Some("explicit"), Some(&env)).as_deref(),
            Some("explicit")
        );
        env.insert("GOOGLE_CLOUD_PROJECT".into(), "google".into());
        assert_eq!(
            resolve_vertex_project_from_env(None, Some(&env)).as_deref(),
            Some("google")
        );
        assert_eq!(
            resolve_vertex_project_from_env(Some(""), Some(&env)).as_deref(),
            Some("google")
        );
        env.insert("GOOGLE_APPLICATION_CREDENTIALS".into(), "/key.json".into());
        assert_eq!(
            build_google_auth_options(Some(&env)),
            Some(GoogleAuthOptions {
                key_filename: "/key.json".into()
            })
        );
    }
}
