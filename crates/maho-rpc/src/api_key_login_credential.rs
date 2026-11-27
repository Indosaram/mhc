//! Port of senpi `packages/coding-agent/src/modes/rpc/api-key-login-credential.ts`.
//!
//! The credential an RPC `login_api_key` stores. With a `region`, the provider's own interactive
//! API-key login runs with the region and key as its answers, so the stored credential is exactly
//! what the CLI's `/login` stores for the same choices. Without one it is the plain key.

use async_trait::async_trait;
use maho_ai::auth::types::{
    ApiKeyCredential, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind,
    ProviderAuthInteraction,
};
use maho_ai::models::Provider;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const REGION_LOGIN_TIMEOUT: Duration = Duration::from_secs(15);

struct RegionLoginInteraction {
    provider_name: String,
    key: String,
    region: String,
    region_asked: AtomicBool,
    signal: AbortSignal,
}

#[async_trait]
impl AuthInteraction for RegionLoginInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        match prompt.kind {
            AuthPromptKind::Secret { .. } => Ok(self.key.clone()),
            AuthPromptKind::Select { options, .. } => {
                // Only one select is the region: a provider asking a second one is asking something else.
                if self.region_asked.swap(true, Ordering::SeqCst) {
                    anyhow::bail!(
                        "{} asked for more than a key and a region.",
                        self.provider_name
                    );
                }
                if !options.iter().any(|opt| opt.id == self.region) {
                    anyhow::bail!(
                        "Unknown region \"{}\" for {}.",
                        self.region,
                        self.provider_name
                    );
                }
                Ok(self.region.clone())
            }
            _ => anyhow::bail!(
                "{} asked for more than a key and a region.",
                self.provider_name
            ),
        }
    }

    fn notify(&self, _event: AuthEvent) {}
}

pub async fn api_key_login_credential(
    provider: Option<&dyn Provider>,
    key: &str,
    region: Option<&str>,
) -> anyhow::Result<ApiKeyCredential> {
    let Some(region) = region else {
        return Ok(ApiKeyCredential {
            key: Some(key.to_owned()),
            ..Default::default()
        });
    };
    let provider_name = provider.map(Provider::name).unwrap_or("This provider");
    let Some(api_key_auth) = provider
        .and_then(Provider::auth)
        .and_then(|auth| auth.api_key.as_ref())
        .filter(|api_key| api_key.has_login())
    else {
        anyhow::bail!("{provider_name} does not take a region for its API key.");
    };

    let abort = AbortController::new();
    let interaction_impl = Arc::new(RegionLoginInteraction {
        provider_name: provider_name.to_owned(),
        key: key.to_owned(),
        region: region.to_owned(),
        region_asked: AtomicBool::new(false),
        signal: abort.signal(),
    });
    let interaction = ProviderAuthInteraction::new(abort.signal(), interaction_impl.clone());

    let mut credential =
        match tokio::time::timeout(REGION_LOGIN_TIMEOUT, api_key_auth.login(&interaction)).await {
            Ok(res) => res?,
            Err(_) => {
                abort.abort(None);
                anyhow::bail!("{provider_name} API key region login timed out.");
            }
        };

    // The login may settle the region from the host's env and never ask. The key is stored only
    // when that settled region is the one requested; a login with no region at all takes none.
    if !interaction_impl.region_asked.load(Ordering::SeqCst) {
        let Some(env) = &credential.env else {
            anyhow::bail!("{provider_name} does not take a region for its API key.");
        };
        if !env.values().any(|v| v == region) {
            anyhow::bail!(
                "This host's environment sets the {provider_name} region, so \"{region}\" cannot be chosen here."
            );
        }
    }

    credential.key = Some(key.to_owned());
    Ok(credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::auth::types::{
        ApiKeyAuth, AuthContext, AuthPromptOption, AuthResult, ProviderAuth,
    };
    use maho_ai::model::Model;
    use maho_ai::types::ProviderEnv;

    struct RegionAuth {
        ask_region: bool,
        settled_region: Option<&'static str>,
    }

    #[async_trait]
    impl ApiKeyAuth for RegionAuth {
        fn name(&self) -> &str {
            "TestAuth"
        }
        fn has_login(&self) -> bool {
            true
        }
        async fn login(
            &self,
            interaction: &ProviderAuthInteraction,
        ) -> anyhow::Result<ApiKeyCredential> {
            let region = if self.ask_region {
                interaction
                    .prompt(AuthPrompt {
                        kind: AuthPromptKind::Select {
                            message: "Region".into(),
                            options: vec![AuthPromptOption {
                                id: "cn".into(),
                                label: "China".into(),
                                description: None,
                            }],
                        },
                        signal: None,
                    })
                    .await?
            } else {
                self.settled_region.unwrap_or("global").to_owned()
            };
            let key = interaction
                .prompt(AuthPrompt {
                    kind: AuthPromptKind::Secret {
                        message: "Key".into(),
                        placeholder: None,
                    },
                    signal: None,
                })
                .await?;
            let mut env = ProviderEnv::new();
            env.insert("REGION".into(), region);
            Ok(ApiKeyCredential {
                key: Some(key),
                env: Some(env),
                ..Default::default()
            })
        }
        async fn resolve(
            &self,
            _: &dyn AuthContext,
            _: Option<&ApiKeyCredential>,
            _: &AbortSignal,
        ) -> anyhow::Result<Option<AuthResult>> {
            Ok(None)
        }
    }

    struct DummyProvider(ProviderAuth);
    impl Provider for DummyProvider {
        fn id(&self) -> &str {
            "kimi"
        }
        fn name(&self) -> &str {
            "Kimi"
        }
        fn auth(&self) -> Option<&ProviderAuth> {
            Some(&self.0)
        }
        fn get_models(&self) -> Vec<Model> {
            Vec::new()
        }
        fn stream(
            &self,
            _model: &Model,
            _context: &maho_ai::types::Context,
            _options: Option<maho_ai::types::StreamOptions>,
        ) -> maho_ai::utils::event_stream::AssistantMessageEventStream {
            unimplemented!("unused in test")
        }
        fn stream_simple(
            &self,
            _model: &Model,
            _context: &maho_ai::types::Context,
            _options: Option<maho_ai::types::SimpleStreamOptions>,
        ) -> maho_ai::utils::event_stream::AssistantMessageEventStream {
            unimplemented!("unused in test")
        }
    }

    #[tokio::test]
    async fn region_login_stores_region_env_and_rejects_mismatched_env_override() {
        let cred_plain = api_key_login_credential(None, "sk-1", None)
            .await
            .expect("plain");
        assert_eq!(cred_plain.key.as_deref(), Some("sk-1"));

        let provider = DummyProvider(ProviderAuth {
            api_key: Some(Arc::new(RegionAuth {
                ask_region: true,
                settled_region: None,
            })),
            oauth: None,
        });
        let cred = api_key_login_credential(Some(&provider), "sk-2", Some("cn"))
            .await
            .expect("region login");
        assert_eq!(cred.key.as_deref(), Some("sk-2"));
        assert_eq!(
            cred.env
                .as_ref()
                .and_then(|e| e.get("REGION"))
                .map(String::as_str),
            Some("cn")
        );

        let unknown = api_key_login_credential(Some(&provider), "sk-2", Some("us"))
            .await
            .unwrap_err();
        assert!(unknown.to_string().contains("Unknown region"));

        let fixed_env = DummyProvider(ProviderAuth {
            api_key: Some(Arc::new(RegionAuth {
                ask_region: false,
                settled_region: Some("global"),
            })),
            oauth: None,
        });
        let mismatch = api_key_login_credential(Some(&fixed_env), "sk-2", Some("cn"))
            .await
            .unwrap_err();
        assert!(mismatch.to_string().contains("cannot be chosen here"));
    }
}
