//! Port of senpi imagegen/openai-auth.ts.
use crate::auth::{ImageGenAuthRegistry, ImageGenResolved};
use crate::credentials::{OPENAI_BASE_URL, credential_parts, non_empty, same_base_url};

/// How explicitly the user configured the `openai` provider. `Store` (a stored or runtime key)
/// keeps its lead ahead of PI_IMAGE_GEN_PROVIDER; `ProviderConfig` (a models.json override) ranks
/// after the pin. A key that only comes from OPENAI_API_KEY is neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiTier {
    Store,
    ProviderConfig,
}
impl OpenAiTier {
    const fn provenance(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::ProviderConfig => "provider-config",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredOpenAi {
    pub tier: OpenAiTier,
    pub resolution: ImageGenResolved,
}

fn openai_tier(stored: bool, base_url: &str, status_source: Option<&str>) -> Option<OpenAiTier> {
    if stored || status_source == Some("runtime") {
        return Some(OpenAiTier::Store);
    }
    if !same_base_url(base_url, OPENAI_BASE_URL) {
        return Some(OpenAiTier::ProviderConfig);
    }
    if status_source.is_some_and(|source| source.starts_with("models_json")) {
        return Some(OpenAiTier::ProviderConfig);
    }
    None
}

async fn stored_key_without_catalog_model(
    registry: &dyn ImageGenAuthRegistry,
) -> Option<ConfiguredOpenAi> {
    let (api_key, headers) =
        credential_parts(registry.get_provider_auth("openai").await.ok().flatten())?;
    Some(ConfiguredOpenAi {
        tier: OpenAiTier::Store,
        resolution: ImageGenResolved {
            kind: "native-openai",
            api_key,
            headers,
            base_url: OPENAI_BASE_URL.into(),
            provenance: "store",
            provider_id: Some("openai".into()),
        },
    })
}

pub async fn resolve_configured_openai(
    registry: &dyn ImageGenAuthRegistry,
) -> Option<ConfiguredOpenAi> {
    let stored = registry.stored_openai_is_api_key();
    let model = registry.get_all().into_iter().find(|candidate| {
        candidate.provider == "openai" && non_empty(Some(&candidate.base_url)).is_some()
    });
    let Some(model) = model else {
        return if stored {
            stored_key_without_catalog_model(registry).await
        } else {
            None
        };
    };
    let source = registry.get_provider_auth_source("openai");
    let tier = openai_tier(stored, &model.base_url, source.as_deref())?;
    let resolved = registry
        .get_api_key_and_headers(&model)
        .await
        .ok()
        .flatten()?;
    let base_url = non_empty(resolved.base_url.as_deref())
        .map_or_else(|| model.base_url.clone(), str::to_owned);
    let (api_key, headers) = credential_parts(Some(resolved))?;
    Some(ConfiguredOpenAi {
        tier,
        resolution: ImageGenResolved {
            kind: if same_base_url(&base_url, OPENAI_BASE_URL) {
                "native-openai"
            } else {
                "gateway"
            },
            api_key,
            headers,
            base_url,
            provenance: tier.provenance(),
            provider_id: Some("openai".into()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tier_ranks_store_before_provider_config_and_env_has_none() {
        assert_eq!(
            openai_tier(true, OPENAI_BASE_URL, None),
            Some(OpenAiTier::Store)
        );
        assert_eq!(
            openai_tier(false, OPENAI_BASE_URL, Some("runtime")),
            Some(OpenAiTier::Store)
        );
        assert_eq!(
            openai_tier(false, "https://gw.example/v1", None),
            Some(OpenAiTier::ProviderConfig)
        );
        assert_eq!(
            openai_tier(false, "https://api.openai.com/v1/", Some("models_json_key")),
            Some(OpenAiTier::ProviderConfig)
        );
        assert_eq!(
            openai_tier(false, OPENAI_BASE_URL, Some("environment")),
            None
        );
    }
}
