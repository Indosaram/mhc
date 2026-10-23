use maho_ai::model::Model;
use std::{collections::BTreeMap, future::Future, pin::Pin};

pub type AuthFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Credentials>, String>> + Send + 'a>>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, Option<String>>,
    pub base_url: Option<String>,
}
pub trait ImageGenAuthRegistry: Send + Sync {
    fn stored_openai_is_api_key(&self) -> bool;
    fn get_all(&self) -> Vec<Model>;
    /// senpi `registry.getProviderAuthStatus(provider).source`; absent accessors report `None`.
    fn get_provider_auth_source(&self, _provider: &str) -> Option<String> {
        None
    }
    fn get_provider_auth(&self, provider: &str) -> AuthFuture<'_>;
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> AuthFuture<'a>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageGenAuthResolution {
    Configured {
        kind: &'static str,
        api_key: String,
        base_url: String,
        headers: BTreeMap<String, String>,
        provenance: &'static str,
        provider_id: Option<String>,
    },
    None {
        reason: &'static str,
    },
}
pub use crate::credentials::OPENAI_BASE_URL;
use crate::credentials::{credential_parts, is_placeholder_key, non_empty};
pub const SETUP_REASON: &str = "Image generation is not configured. Store an OpenAI API key for provider \"openai\", configure an OpenAI-compatible gateway in models.json and optionally pin it with PI_IMAGE_GEN_PROVIDER, or set OPENAI_API_KEY.";
pub async fn resolve_context_image_gen_auth(
    ctx: &maho_ext_api::ExtensionContext,
) -> Result<ImageGenAuthResolution, maho_ext_api::ExtensionFailure> {
    let registry = if let Some(registry) = crate::state::image_gen_registry_override() {
        registry
    } else {
        std::sync::Arc::new(ContextAuthRegistry {
            stored_api_key: ctx.model_registry.get_stored_credential_type("openai")?
                == Some(maho_ai::auth::types::CredentialType::ApiKey),
            openai_source: ctx.model_registry.get_provider_auth_status("openai").source,
            registry: ctx.model_registry.clone(),
        }) as std::sync::Arc<dyn ImageGenAuthRegistry>
    };
    Ok(resolve_image_gen_auth(registry.as_ref(), &std::env::vars().collect()).await)
}
/// Builds the ordered candidate stream for the context registry (or the test override).
pub fn context_image_gen_candidates(
    ctx: &maho_ext_api::ExtensionContext,
) -> Result<ImageGenCandidates<'static>, maho_ext_api::ExtensionFailure> {
    let registry = if let Some(registry) = crate::state::image_gen_registry_override() {
        registry
    } else {
        std::sync::Arc::new(ContextAuthRegistry {
            stored_api_key: ctx.model_registry.get_stored_credential_type("openai")?
                == Some(maho_ai::auth::types::CredentialType::ApiKey),
            openai_source: ctx.model_registry.get_provider_auth_status("openai").source,
            registry: ctx.model_registry.clone(),
        }) as std::sync::Arc<dyn ImageGenAuthRegistry>
    };
    Ok(image_gen_candidates(registry, std::env::vars().collect()))
}
struct ContextAuthRegistry {
    stored_api_key: bool,
    openai_source: Option<String>,
    registry: std::sync::Arc<dyn maho_ext_api::ModelRegistry>,
}
impl ImageGenAuthRegistry for ContextAuthRegistry {
    fn stored_openai_is_api_key(&self) -> bool {
        self.stored_api_key
    }
    fn get_all(&self) -> Vec<Model> {
        self.registry.get_all()
    }
    fn get_provider_auth_source(&self, provider: &str) -> Option<String> {
        if provider == "openai" {
            self.openai_source.clone()
        } else {
            None
        }
    }
    fn get_provider_auth(&self, provider: &str) -> AuthFuture<'_> {
        let provider = provider.to_owned();
        Box::pin(async move {
            self.registry
                .get_provider_auth(&provider)
                .await
                .map(|auth| {
                    auth.map(|auth| Credentials {
                        api_key: auth.auth.api_key,
                        headers: auth.auth.headers.unwrap_or_default(),
                        base_url: auth.auth.base_url,
                    })
                })
                .map_err(|error| error.message)
        })
    }
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> AuthFuture<'a> {
        Box::pin(async move {
            self.registry
                .get_api_key_and_headers(model)
                .await
                .map(|auth| {
                    Some(Credentials {
                        api_key: auth.auth.api_key,
                        headers: auth.auth.headers.unwrap_or_default(),
                        base_url: auth.auth.base_url,
                    })
                })
                .map_err(|error| error.message)
        })
    }
}
/// A resolved image source (senpi `ImageGenResolvedAuth`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGenResolved {
    pub kind: &'static str,
    pub api_key: String,
    pub base_url: String,
    pub headers: BTreeMap<String, String>,
    pub provenance: &'static str,
    pub provider_id: Option<String>,
}
/// One source the tool may send the request to. `fallthrough` is true only for a gateway found by
/// scanning models.json: when it has no images endpoint (404/405) the tool tries the next candidate.
/// Sources the user chose (stored/overridden `openai`, the pin, OPENAI_API_KEY) never fall through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGenCandidate {
    pub resolved: ImageGenResolved,
    pub fallthrough: bool,
}
impl ImageGenCandidate {
    pub fn into_resolution(self) -> ImageGenAuthResolution {
        let ImageGenResolved {
            kind,
            api_key,
            base_url,
            headers,
            provenance,
            provider_id,
        } = self.resolved;
        ImageGenAuthResolution::Configured {
            kind,
            api_key,
            base_url,
            headers,
            provenance,
            provider_id,
        }
    }
}
enum Stage {
    Start,
    Gateways,
    Env,
    Done,
}
/// Ordered, lazy candidate stream: a stored `openai` key, the PI_IMAGE_GEN_PROVIDER pin, a models.json
/// `openai` override, scanned gateways, then OPENAI_API_KEY. Credentials resolve only when reached.
pub struct ImageGenCandidates<'a> {
    registry: std::sync::Arc<dyn ImageGenAuthRegistry + 'a>,
    env: BTreeMap<String, String>,
    stage: Stage,
    groups: BTreeMap<String, Model>,
    pinned: Option<String>,
    queue: std::collections::VecDeque<String>,
}
pub fn image_gen_candidates<'a>(
    registry: std::sync::Arc<dyn ImageGenAuthRegistry + 'a>,
    env: BTreeMap<String, String>,
) -> ImageGenCandidates<'a> {
    ImageGenCandidates {
        registry,
        env,
        stage: Stage::Start,
        groups: BTreeMap::new(),
        pinned: None,
        queue: Default::default(),
    }
}
async fn resolve_gateway(
    provider: &str,
    model: &Model,
    registry: &dyn ImageGenAuthRegistry,
) -> Option<ImageGenResolved> {
    let base_url = non_empty(Some(&model.base_url))?.to_owned();
    let (api_key, headers) =
        credential_parts(registry.get_api_key_and_headers(model).await.ok().flatten())?;
    Some(ImageGenResolved {
        kind: "gateway",
        api_key,
        headers,
        base_url,
        provenance: "provider-config",
        provider_id: Some(provider.into()),
    })
}
impl ImageGenCandidates<'_> {
    pub async fn next(&mut self) -> Option<ImageGenCandidate> {
        loop {
            match self.stage {
                Stage::Done => return None,
                Stage::Start => {
                    let openai =
                        crate::openai_auth::resolve_configured_openai(self.registry.as_ref()).await;
                    if let Some(openai) = openai
                        .as_ref()
                        .filter(|openai| openai.tier == crate::openai_auth::OpenAiTier::Store)
                    {
                        self.stage = Stage::Done;
                        return Some(ImageGenCandidate {
                            resolved: openai.resolution.clone(),
                            fallthrough: false,
                        });
                    }
                    let mut models = self.registry.get_all();
                    models.sort_by(|left, right| {
                        left.provider
                            .cmp(&right.provider)
                            .then(left.id.cmp(&right.id))
                    });
                    for model in models {
                        if model.provider != "openai"
                            && matches!(
                                model.api.as_str(),
                                "openai-completions" | "openai-responses"
                            )
                            && non_empty(Some(&model.base_url)).is_some()
                        {
                            self.groups.entry(model.provider.clone()).or_insert(model);
                        }
                    }
                    self.pinned =
                        non_empty(self.env.get("PI_IMAGE_GEN_PROVIDER").map(String::as_str))
                            .map(str::to_owned);
                    if let Some(pinned) = &self.pinned
                        && let Some(model) = self.groups.get(pinned)
                        && let Some(resolved) =
                            resolve_gateway(pinned, model, self.registry.as_ref()).await
                    {
                        self.stage = Stage::Done;
                        return Some(ImageGenCandidate {
                            resolved,
                            fallthrough: false,
                        });
                    }
                    if let Some(openai) = openai {
                        self.stage = Stage::Done;
                        return Some(ImageGenCandidate {
                            resolved: openai.resolution,
                            fallthrough: false,
                        });
                    }
                    let mut providers = self
                        .groups
                        .keys()
                        .filter(|provider| Some(provider.as_str()) != self.pinned.as_deref())
                        .cloned()
                        .collect::<Vec<_>>();
                    providers.sort_by(|left, right| {
                        (!left.to_ascii_lowercase().contains("openai"))
                            .cmp(&(!right.to_ascii_lowercase().contains("openai")))
                            .then(left.cmp(right))
                    });
                    self.queue = providers.into();
                    self.stage = Stage::Gateways;
                }
                Stage::Gateways => {
                    let Some(provider) = self.queue.pop_front() else {
                        self.stage = Stage::Env;
                        continue;
                    };
                    let Some(model) = self.groups.get(&provider) else {
                        continue;
                    };
                    if let Some(resolved) =
                        resolve_gateway(&provider, model, self.registry.as_ref()).await
                    {
                        return Some(ImageGenCandidate {
                            resolved,
                            fallthrough: true,
                        });
                    }
                }
                Stage::Env => {
                    self.stage = Stage::Done;
                    let key = non_empty(self.env.get("OPENAI_API_KEY").map(String::as_str))?;
                    if is_placeholder_key(key) {
                        return None;
                    }
                    return Some(ImageGenCandidate {
                        resolved: ImageGenResolved {
                            kind: "native-openai",
                            api_key: key.into(),
                            base_url: OPENAI_BASE_URL.into(),
                            headers: BTreeMap::new(),
                            provenance: "env",
                            provider_id: None,
                        },
                        fallthrough: false,
                    });
                }
            }
        }
    }
}
/// First candidate, or `None` with the setup reason.
pub async fn resolve_image_gen_auth(
    registry: &dyn ImageGenAuthRegistry,
    env: &BTreeMap<String, String>,
) -> ImageGenAuthResolution {
    let first = ImageGenCandidates {
        registry: std::sync::Arc::new(BorrowedRegistry(registry)),
        env: env.clone(),
        stage: Stage::Start,
        groups: BTreeMap::new(),
        pinned: None,
        queue: Default::default(),
    }
    .next()
    .await;
    first.map_or(
        ImageGenAuthResolution::None {
            reason: SETUP_REASON,
        },
        ImageGenCandidate::into_resolution,
    )
}
struct BorrowedRegistry<'a>(&'a dyn ImageGenAuthRegistry);
impl ImageGenAuthRegistry for BorrowedRegistry<'_> {
    fn stored_openai_is_api_key(&self) -> bool {
        self.0.stored_openai_is_api_key()
    }
    fn get_all(&self) -> Vec<Model> {
        self.0.get_all()
    }
    fn get_provider_auth_source(&self, provider: &str) -> Option<String> {
        self.0.get_provider_auth_source(provider)
    }
    fn get_provider_auth(&self, provider: &str) -> AuthFuture<'_> {
        self.0.get_provider_auth(provider)
    }
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> AuthFuture<'a> {
        self.0.get_api_key_and_headers(model)
    }
}
