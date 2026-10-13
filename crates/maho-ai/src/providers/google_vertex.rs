//! Port of senpi packages/ai/src/providers/google-vertex.ts.

use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthEvent, AuthInfoLink, AuthPrompt, AuthPromptKind, AuthPromptOption,
    AuthResult, ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};
use std::sync::Arc;

pub fn stream_google_vertex(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    builtin_api_streams("google-vertex").stream(model, context, options)
}

pub fn stream_simple_google_vertex(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("google-vertex").stream_simple(model, context, options)
}


fn select_option(id: &str, label: &str) -> AuthPromptOption {
    AuthPromptOption { id: id.into(), label: label.into(), description: None }
}

async fn prompt(interaction: &ProviderAuthInteraction, kind: AuthPromptKind) -> anyhow::Result<String> {
    interaction.prompt(AuthPrompt { kind, signal: None }).await
}

fn aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))
}

/// The `env(name)` closure of the TS resolver: abort-checked on both sides of the lookup.
async fn env_value(ctx: &dyn AuthContext, name: &str, signal: &AbortSignal) -> anyhow::Result<Option<String>> {
    aborted(signal)?;
    let value = ctx.env(name).await;
    aborted(signal)?;
    Ok(value.filter(|value| !value.is_empty()))
}

const VERTEX_ADC_PATH: &str = "~/.config/gcloud/application_default_credentials.json";

/// Vertex accepts an explicit API key or Application Default Credentials
/// (`gcloud auth application-default login`). ADC additionally requires project and location env
/// vars, which the implementation reads itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct VertexAuth;

#[async_trait]
impl ApiKeyAuth for VertexAuth {
    fn name(&self) -> &str {
        "Google Cloud credentials"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        aborted(&interaction.signal)?;
        let method = prompt(
            interaction,
            AuthPromptKind::Select {
                message: "Select Google Vertex AI authentication method:".into(),
                options: vec![
                    select_option("api-key", "Google Cloud API key"),
                    select_option("adc", "Application Default Credentials"),
                    select_option("service-account", "Service account credentials file"),
                ],
            },
        )
        .await?;
        aborted(&interaction.signal)?;
        if method == "api-key" {
            let key =
                prompt(interaction, AuthPromptKind::Secret { message: "Enter Google Cloud API key".into(), placeholder: None }).await?;
            return Ok(ApiKeyCredential { key: Some(key), env: None });
        }
        if method != "adc" && method != "service-account" {
            anyhow::bail!("Unknown Google Vertex AI auth method: {method}");
        }
        interaction.notify(AuthEvent::Info {
            message: if method == "adc" {
                "Run `gcloud auth application-default login`, then provide the project and location.".into()
            } else {
                "Provide a service account credentials file, project, and location.".into()
            },
            links: Some(vec![AuthInfoLink {
                url: "https://cloud.google.com/docs/authentication/provide-credentials-adc".into(),
                label: Some("Application Default Credentials".into()),
            }]),
        });
        let project =
            prompt(interaction, AuthPromptKind::Text { message: "Enter Google Cloud project ID".into(), placeholder: None }).await?;
        let location =
            prompt(interaction, AuthPromptKind::Text { message: "Enter Google Cloud location".into(), placeholder: None }).await?;
        let credentials_path = if method == "service-account" {
            Some(
                prompt(
                    interaction,
                    AuthPromptKind::Text { message: "Enter service account credentials file path".into(), placeholder: None },
                )
                .await?,
            )
        } else {
            None
        };
        let mut env = crate::types::ProviderEnv::new();
        env.insert("GOOGLE_CLOUD_PROJECT".into(), project);
        env.insert("GOOGLE_CLOUD_LOCATION".into(), location);
        if let Some(path) = credentials_path.filter(|path| !path.is_empty()) {
            env.insert("GOOGLE_APPLICATION_CREDENTIALS".into(), path);
        }
        Ok(ApiKeyCredential { key: None, env: Some(env) })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        let stored_key = credential.and_then(|c| c.key.clone());
        let key = match &stored_key {
            Some(key) => Some(key.clone()),
            None => env_value(ctx, "GOOGLE_CLOUD_API_KEY", signal).await?,
        };
        if let Some(key) = key.filter(|key| !key.is_empty()) {
            let source = if stored_key.is_some() { "stored credential" } else { "GOOGLE_CLOUD_API_KEY" };
            return Ok(Some(AuthResult {
                auth: ModelAuth { api_key: Some(key), ..Default::default() },
                env: None,
                source: Some(source.into()),
            }));
        }

        let stored_env = |name: &str| credential.and_then(|c| c.env.as_ref()).and_then(|env| env.get(name)).cloned();
        let adc_path = match stored_env("GOOGLE_APPLICATION_CREDENTIALS") {
            Some(path) => Some(path),
            None => env_value(ctx, "GOOGLE_APPLICATION_CREDENTIALS", signal).await?,
        };
        aborted(signal)?;
        let has_credentials = ctx.file_exists(adc_path.as_deref().unwrap_or(VERTEX_ADC_PATH)).await;
        aborted(signal)?;
        let project = match stored_env("GOOGLE_CLOUD_PROJECT") {
            Some(project) => Some(project),
            None => match env_value(ctx, "GOOGLE_CLOUD_PROJECT", signal).await? {
                Some(project) => Some(project),
                None => env_value(ctx, "GCLOUD_PROJECT", signal).await?,
            },
        };
        let location = match stored_env("GOOGLE_CLOUD_LOCATION") {
            Some(location) => Some(location),
            None => env_value(ctx, "GOOGLE_CLOUD_LOCATION", signal).await?,
        };
        if has_credentials && project.is_some_and(|p| !p.is_empty()) && location.is_some_and(|l| !l.is_empty()) {
            let source = if credential.is_some() { "stored credential" } else { "gcloud application default credentials" };
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: credential.and_then(|c| c.env.clone()),
                source: Some(source.into()),
            }));
        }
        Ok(None)
    }
}

pub fn google_vertex_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "google-vertex".into(),
        name: Some("Google Vertex AI".into()),
        base_url: None,
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(VertexAuth)), oauth: None }),
        models: super::google_vertex_models::google_vertex_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("google-vertex")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Env {
        values: HashMap<String, String>,
        files: Vec<String>,
    }

    #[async_trait]
    impl AuthContext for Env {
        async fn env(&self, name: &str) -> Option<String> {
            self.values.get(name).cloned()
        }
        async fn file_exists(&self, path: &str) -> bool {
            self.files.iter().any(|file| file == path)
        }
    }

    #[tokio::test]
    async fn stored_adc_project_and_location_resolve_with_their_env() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let env: crate::types::ProviderEnv = [
            ("GOOGLE_CLOUD_PROJECT".to_owned(), "proj".to_owned()),
            ("GOOGLE_CLOUD_LOCATION".to_owned(), "us-central1".to_owned()),
            ("GOOGLE_APPLICATION_CREDENTIALS".to_owned(), "/sa.json".to_owned()),
        ]
        .into();
        let credential = ApiKeyCredential { key: None, env: Some(env.clone()) };
        let ctx = Env { values: HashMap::new(), files: vec!["/sa.json".into()] };
        let result = VertexAuth.resolve(&ctx, Some(&credential), &signal).await.expect("resolve").expect("configured");
        assert_eq!(result.source.as_deref(), Some("stored credential"));
        assert_eq!(result.env, Some(env));
    }

    #[tokio::test]
    async fn adc_without_location_is_unconfigured() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let ctx = Env {
            values: [("GOOGLE_CLOUD_PROJECT".to_owned(), "proj".to_owned())].into(),
            files: vec![VERTEX_ADC_PATH.into()],
        };
        assert!(VertexAuth.resolve(&ctx, None, &signal).await.expect("resolve").is_none());
    }
}
