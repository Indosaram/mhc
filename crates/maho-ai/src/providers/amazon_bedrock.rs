//! Port of senpi packages/ai/src/providers/amazon-bedrock.ts.

use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthEvent, AuthInfoLink, AuthPrompt, AuthPromptKind, AuthPromptOption,
    AuthResult, ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;


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

fn stored(credential: Option<&ApiKeyCredential>, source: &str) -> Option<AuthResult> {
    Some(AuthResult { auth: ModelAuth::default(), env: credential.and_then(|c| c.env.clone()), source: Some(source.into()) })
}

fn ambient(source: &str) -> Option<AuthResult> {
    Some(AuthResult { auth: ModelAuth::default(), env: None, source: Some(source.into()) })
}

/// Bedrock accepts a bearer token or the AWS SDK's default credential chain. The login flow can
/// store a token/profile choice; resolve also detects ambient AWS credentials without copying them
/// into the credential store.
#[derive(Debug, Clone, Copy, Default)]
pub struct BedrockAuth;

#[async_trait]
impl ApiKeyAuth for BedrockAuth {
    fn name(&self) -> &str {
        "AWS credentials or bearer token"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        aborted(&interaction.signal)?;
        let method = prompt(
            interaction,
            AuthPromptKind::Select {
                message: "Select Amazon Bedrock authentication method:".into(),
                options: vec![
                    select_option("bearer-token", "Bearer token"),
                    select_option("aws-profile", "AWS profile"),
                    select_option("credential-chain", "Existing AWS credential chain"),
                ],
            },
        )
        .await?;
        aborted(&interaction.signal)?;
        if method == "bearer-token" {
            let key = prompt(
                interaction,
                AuthPromptKind::Secret { message: "Enter Amazon Bedrock bearer token".into(), placeholder: None },
            )
            .await?;
            return Ok(ApiKeyCredential { key: Some(key), env: None });
        }
        interaction.notify(AuthEvent::Info {
            message: "Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials.".into(),
            links: Some(vec![AuthInfoLink {
                url: "https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html".into(),
                label: Some("AWS credential provider chain".into()),
            }]),
        });
        if method == "aws-profile" {
            let profile =
                prompt(interaction, AuthPromptKind::Text { message: "Enter AWS profile name".into(), placeholder: None }).await?;
            return Ok(ApiKeyCredential { key: None, env: Some([("AWS_PROFILE".to_owned(), profile)].into()) });
        }
        if method != "credential-chain" {
            anyhow::bail!("Unknown Amazon Bedrock auth method: {method}");
        }
        prompt(
            interaction,
            AuthPromptKind::Text { message: "Configure AWS credentials, then press Enter to continue".into(), placeholder: None },
        )
        .await?;
        Ok(ApiKeyCredential::default())
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        if let Some(credential) = credential
            && let Some(key) = credential.key.as_ref().filter(|key| !key.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth { api_key: Some(key.clone()), ..Default::default() },
                env: credential.env.clone(),
                source: Some("stored credential".into()),
            }));
        }
        if env_value(ctx, "AWS_BEARER_TOKEN_BEDROCK", signal).await?.is_some() {
            return Ok(ambient("AWS_BEARER_TOKEN_BEDROCK"));
        }
        let stored_profile = credential.and_then(|c| c.env.as_ref()).and_then(|env| env.get("AWS_PROFILE")).filter(|p| !p.is_empty());
        if stored_profile.is_some() {
            return Ok(stored(credential, "stored credential"));
        }
        if env_value(ctx, "AWS_PROFILE", signal).await?.is_some() {
            return Ok(stored(credential, "AWS_PROFILE"));
        }
        if env_value(ctx, "AWS_ACCESS_KEY_ID", signal).await?.is_some()
            && env_value(ctx, "AWS_SECRET_ACCESS_KEY", signal).await?.is_some()
        {
            return Ok(ambient("AWS access keys"));
        }
        if env_value(ctx, "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", signal).await?.is_some() {
            return Ok(ambient("ECS task role"));
        }
        if env_value(ctx, "AWS_CONTAINER_CREDENTIALS_FULL_URI", signal).await?.is_some() {
            return Ok(ambient("ECS task role"));
        }
        if env_value(ctx, "AWS_WEB_IDENTITY_TOKEN_FILE", signal).await?.is_some() {
            return Ok(ambient("web identity token"));
        }
        Ok(None)
    }
}

pub fn amazon_bedrock_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "amazon-bedrock".into(),
        name: Some("Amazon Bedrock".into()),
        base_url: None,
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(BedrockAuth)), oauth: None }),
        models: super::amazon_bedrock_models::amazon_bedrock_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("bedrock-converse-stream")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Env(HashMap<String, String>);

    #[async_trait]
    impl AuthContext for Env {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn stored_profile_env_is_returned_with_the_resolution() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let credential = ApiKeyCredential { key: None, env: Some([("AWS_PROFILE".to_owned(), "work".to_owned())].into()) };
        let result = BedrockAuth.resolve(&Env(HashMap::new()), Some(&credential), &signal).await.expect("resolve").expect("configured");
        assert_eq!(result.source.as_deref(), Some("stored credential"));
        assert_eq!(result.env.expect("env").get("AWS_PROFILE").map(String::as_str), Some("work"));
    }

    #[tokio::test]
    async fn no_aws_configuration_is_unconfigured() {
        let signal = crate::utils::abort::AbortController::new().signal();
        assert!(BedrockAuth.resolve(&Env(HashMap::new()), None, &signal).await.expect("resolve").is_none());
    }
}
