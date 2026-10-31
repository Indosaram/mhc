use anyhow::Result;
use maho_ai::{
    auth::types::{ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult},
    types::ProviderEnv,
    utils::abort::AbortSignal,
};
use std::{future::Future, pin::Pin, sync::Arc};

/// The ADC file `gcloud auth application-default login` writes, same as the builtin google-vertex provider.
pub const VERTEX_MODEL_GARDEN_ADC_PATH: &str =
    "~/.config/gcloud/application_default_credentials.json";
pub const VERTEX_MODEL_GARDEN_ADC_SOURCE: &str = "gcloud application default credentials";

/// Read-only env of another provider's stored credential; routing metadata only, never keys.
pub type StoredProviderEnvReader =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<ProviderEnv>> + Send>> + Send + Sync>;

fn check_aborted(signal: &AbortSignal) -> Result<()> {
    if signal.aborted() {
        anyhow::bail!("This operation was aborted");
    }
    Ok(())
}

/// Availability through the shared Google Application Default Credentials chain.
/// This is deliberately ambient (senpi#2327): the credentials exist for other gcloud tooling too, so the
/// lane is selectable but never outranks a provider the user configured, and it reports nothing while the
/// chain is absent: the failure belongs at selection time, not at the first request.
///
/// The target project is resolved here so it rides the ambient auth result into the request env: process
/// env first, then the sibling google-vertex profile, which shares this ADC chain and already names the
/// project the user entered at its login. With neither source the lane stays available and the
/// request-time ADC fallbacks (quota project, discovered project) decide, so a missing project remains a
/// first-request failure with the actionable error, not a hidden lane.
pub async fn resolve_vertex_model_garden_ambient(
    ctx: &dyn AuthContext,
    signal: &AbortSignal,
    stored_provider_env: Option<&StoredProviderEnvReader>,
) -> Result<Option<AuthResult>> {
    check_aborted(signal)?;
    let path = ctx
        .env("GOOGLE_APPLICATION_CREDENTIALS")
        .await
        .unwrap_or_else(|| VERTEX_MODEL_GARDEN_ADC_PATH.into());
    check_aborted(signal)?;
    if !ctx.file_exists(&path).await {
        return Ok(None);
    }
    check_aborted(signal)?;
    let mut project = ctx.env("GOOGLE_CLOUD_PROJECT").await;
    if project.is_none() {
        project = ctx.env("GCLOUD_PROJECT").await;
    }
    if project.is_none() {
        // An injected reader overrides the auth context's own stored-credential reader.
        let stored = match stored_provider_env {
            Some(reader) => reader("google-vertex".into()).await,
            None => ctx.stored_provider_env("google-vertex").await,
        };
        project = stored.and_then(|env| env.get("GOOGLE_CLOUD_PROJECT").cloned());
    }
    let env = project
        .filter(|project| !project.is_empty())
        .map(|project| ProviderEnv::from([("GOOGLE_CLOUD_PROJECT".to_owned(), project)]));
    Ok(Some(AuthResult {
        auth: Default::default(),
        env,
        source: Some(VERTEX_MODEL_GARDEN_ADC_SOURCE.into()),
        ambient: true,
    }))
}

/// The `ambientAuth` registration of the `vertex-garden` provider. With no injected reader the
/// stored google-vertex profile is read through `AuthContext::stored_provider_env`.
pub struct VertexModelGardenAmbientAuth {
    pub stored_provider_env: Option<StoredProviderEnvReader>,
}

#[async_trait::async_trait]
impl ApiKeyAuth for VertexModelGardenAmbientAuth {
    fn name(&self) -> &str {
        "Google Cloud credentials (ADC)"
    }
    fn ambient_only(&self) -> bool {
        true
    }
    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> Result<Option<AuthResult>> {
        resolve_vertex_model_garden_ambient(ctx, signal, self.stored_provider_env.as_ref()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Ctx {
        env: BTreeMap<&'static str, &'static str>,
        file: bool,
    }
    #[async_trait::async_trait]
    impl AuthContext for Ctx {
        async fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).map(|value| (*value).to_owned())
        }
        async fn file_exists(&self, _: &str) -> bool {
            self.file
        }
    }
    fn stored(project: &'static str) -> StoredProviderEnvReader {
        Arc::new(move |provider| {
            Box::pin(async move {
                (provider == "google-vertex").then(|| {
                    ProviderEnv::from([("GOOGLE_CLOUD_PROJECT".to_owned(), project.to_owned())])
                })
            })
        })
    }
    fn project(result: Option<AuthResult>) -> Option<String> {
        result
            .and_then(|result| result.env)
            .and_then(|env| env.get("GOOGLE_CLOUD_PROJECT").cloned())
    }

    #[tokio::test]
    async fn absent_adc_chain_reports_nothing() {
        let ctx = Ctx {
            env: BTreeMap::new(),
            file: false,
        };
        assert!(
            resolve_vertex_model_garden_ambient(&ctx, &AbortSignal::default(), None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn project_prefers_env_then_gcloud_env_then_the_stored_google_vertex_profile() {
        let signal = AbortSignal::default();
        let both = Ctx {
            env: BTreeMap::from([
                ("GOOGLE_CLOUD_PROJECT", "env-a"),
                ("GCLOUD_PROJECT", "env-b"),
            ]),
            file: true,
        };
        assert_eq!(
            project(
                resolve_vertex_model_garden_ambient(&both, &signal, Some(&stored("stored")))
                    .await
                    .unwrap()
            )
            .as_deref(),
            Some("env-a")
        );
        let gcloud = Ctx {
            env: BTreeMap::from([("GCLOUD_PROJECT", "env-b")]),
            file: true,
        };
        assert_eq!(
            project(
                resolve_vertex_model_garden_ambient(&gcloud, &signal, Some(&stored("stored")))
                    .await
                    .unwrap()
            )
            .as_deref(),
            Some("env-b")
        );
        let none = Ctx {
            env: BTreeMap::new(),
            file: true,
        };
        assert_eq!(
            project(
                resolve_vertex_model_garden_ambient(&none, &signal, Some(&stored("stored")))
                    .await
                    .unwrap()
            )
            .as_deref(),
            Some("stored")
        );
        let result = resolve_vertex_model_garden_ambient(&none, &signal, None)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.ambient
                && result.env.is_none()
                && result.source.as_deref() == Some(VERTEX_MODEL_GARDEN_ADC_SOURCE)
        );
    }

    struct StoredCtx;
    #[async_trait::async_trait]
    impl AuthContext for StoredCtx {
        async fn env(&self, _: &str) -> Option<String> {
            None
        }
        async fn file_exists(&self, _: &str) -> bool {
            true
        }
        async fn stored_provider_env(&self, provider_id: &str) -> Option<ProviderEnv> {
            (provider_id == "google-vertex").then(|| {
                ProviderEnv::from([("GOOGLE_CLOUD_PROJECT".to_owned(), "profile".to_owned())])
            })
        }
    }

    #[tokio::test]
    async fn default_registration_reads_the_stored_profile_through_the_auth_context() {
        let auth = VertexModelGardenAmbientAuth {
            stored_provider_env: None,
        };
        assert_eq!(
            project(
                auth.resolve(&StoredCtx, None, &AbortSignal::default())
                    .await
                    .unwrap()
            )
            .as_deref(),
            Some("profile")
        );
    }
}
