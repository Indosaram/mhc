//! Port of senpi packages/ai/src/auth/oauth/kimi-region.ts.

use crate::auth::types::{AuthPrompt, AuthPromptKind, AuthPromptOption, ProviderAuthInteraction};
use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KimiCodeRegion {
    MainlandCn,
    Global,
}

impl KimiCodeRegion {
    pub fn id(self) -> &'static str {
        match self {
            KimiCodeRegion::MainlandCn => "mainland-cn",
            KimiCodeRegion::Global => "global",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "mainland-cn" => Some(KimiCodeRegion::MainlandCn),
            "global" => Some(KimiCodeRegion::Global),
            _ => None,
        }
    }
}

pub const KIMI_CODE_REGION_ENV: &str = "KIMI_CODE_REGION";
pub const KIMI_CODE_OAUTH_HOST_ENV: &str = "KIMI_CODE_OAUTH_HOST";
pub const KIMI_OAUTH_HOST_ENV: &str = "KIMI_OAUTH_HOST";
pub const DEFAULT_KIMI_CODE_REGION: KimiCodeRegion = KimiCodeRegion::MainlandCn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KimiCodeRegionProfile {
    pub id: KimiCodeRegion,
    pub label: &'static str,
    pub description: &'static str,
    pub oauth_host: &'static str,
    pub api_base_url: &'static str,
}

pub const KIMI_CODE_REGION_PROFILES: [KimiCodeRegionProfile; 2] = [
    KimiCodeRegionProfile {
        id: KimiCodeRegion::MainlandCn,
        label: "Mainland China (kimi.com)",
        description: "Accounts created on kimi.com",
        oauth_host: "https://auth.kimi.com",
        api_base_url: "https://api.kimi.com/coding",
    },
    KimiCodeRegionProfile {
        id: KimiCodeRegion::Global,
        label: "Outside mainland China (kimi.ai)",
        description: "Accounts created on kimi.ai",
        oauth_host: "https://auth.kimi.ai",
        api_base_url: "https://api.kimi.ai/coding",
    },
];

pub fn kimi_code_region_profile(region: KimiCodeRegion) -> &'static KimiCodeRegionProfile {
    KIMI_CODE_REGION_PROFILES
        .iter()
        .find(|profile| profile.id == region)
        .expect("every region has a profile")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KimiCodeEndpoints {
    pub region: Option<KimiCodeRegion>,
    pub oauth_host: String,
    pub api_base_url: Option<String>,
    pub env: Option<ProviderEnv>,
}

#[derive(Debug, Clone, Default)]
pub struct KimiCodeEndpointInput {
    pub stored_region: Option<String>,
    pub stored_oauth_host: Option<String>,
    pub env_oauth_host: Option<String>,
    pub env_region: Option<String>,
}

pub fn is_kimi_code_region(value: &str) -> bool {
    value == "mainland-cn" || value == "global"
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('/').to_string()
}

pub fn kimi_code_region_for_oauth_host(host: &str) -> Option<KimiCodeRegion> {
    let normalized = normalize_host(host);
    KIMI_CODE_REGION_PROFILES
        .iter()
        .find(|profile| profile.oauth_host == normalized)
        .map(|profile| profile.id)
}

fn endpoints_for_region(region: KimiCodeRegion) -> KimiCodeEndpoints {
    let profile = kimi_code_region_profile(region);
    let mut env = ProviderEnv::new();
    env.insert(KIMI_CODE_REGION_ENV.to_string(), region.id().to_string());
    KimiCodeEndpoints {
        region: Some(region),
        oauth_host: profile.oauth_host.to_string(),
        api_base_url: if region == DEFAULT_KIMI_CODE_REGION { None } else { Some(profile.api_base_url.to_string()) },
        env: Some(env),
    }
}

fn endpoints_for_host(host: &str) -> KimiCodeEndpoints {
    if let Some(region) = kimi_code_region_for_oauth_host(host) {
        return endpoints_for_region(region);
    }
    let normalized = normalize_host(host);
    let mut env = ProviderEnv::new();
    env.insert(KIMI_CODE_OAUTH_HOST_ENV.to_string(), normalized.clone());
    KimiCodeEndpoints { region: None, oauth_host: normalized, api_base_url: None, env: Some(env) }
}

pub fn resolve_kimi_code_endpoints(input: KimiCodeEndpointInput) -> KimiCodeEndpoints {
    if let Some(host) = input.stored_oauth_host.as_deref().filter(|host| !host.trim().is_empty()) {
        return endpoints_for_host(host);
    }
    if let Some(region) = input.stored_region.as_deref().and_then(KimiCodeRegion::from_id) {
        return endpoints_for_region(region);
    }
    if let Some(host) = input.env_oauth_host.as_deref().filter(|host| !host.trim().is_empty()) {
        return endpoints_for_host(host);
    }
    if let Some(region) = input.env_region.as_deref().and_then(KimiCodeRegion::from_id) {
        return endpoints_for_region(region);
    }
    KimiCodeEndpoints { env: None, ..endpoints_for_region(DEFAULT_KIMI_CODE_REGION) }
}

pub fn kimi_code_credential_env(credential: &serde_json::Map<String, serde_json::Value>) -> Option<ProviderEnv> {
    let value = credential.get("env")?;
    let serde_json::Value::Object(entries) = value else {
        return None;
    };
    let mut env = ProviderEnv::new();
    for (name, entry) in entries {
        if let serde_json::Value::String(entry) = entry {
            env.insert(name.clone(), entry.clone());
        }
    }
    Some(env)
}

pub fn kimi_code_endpoints_for_credential(credential: &serde_json::Map<String, serde_json::Value>) -> KimiCodeEndpoints {
    let env = kimi_code_credential_env(credential);
    resolve_kimi_code_endpoints(KimiCodeEndpointInput {
        stored_region: env.as_ref().and_then(|env| env.get(KIMI_CODE_REGION_ENV).cloned()),
        stored_oauth_host: env.as_ref().and_then(|env| env.get(KIMI_CODE_OAUTH_HOST_ENV).cloned()),
        env_oauth_host: get_provider_env_value(KIMI_CODE_OAUTH_HOST_ENV, None)
            .or_else(|| get_provider_env_value(KIMI_OAUTH_HOST_ENV, None)),
        env_region: get_provider_env_value(KIMI_CODE_REGION_ENV, None),
    })
}

pub fn kimi_code_region_prompt() -> AuthPrompt {
    AuthPrompt {
        kind: AuthPromptKind::Select {
            message: "Which Kimi service hosts your account?".into(),
            options: KIMI_CODE_REGION_PROFILES
                .iter()
                .map(|profile| AuthPromptOption {
                    id: profile.id.id().to_string(),
                    label: profile.label.to_string(),
                    description: Some(profile.description.to_string()),
                })
                .collect(),
        },
        signal: None,
    }
}

pub async fn choose_kimi_code_login_endpoints(
    interaction: &ProviderAuthInteraction,
) -> anyhow::Result<KimiCodeEndpoints> {
    let from_env = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
        env_oauth_host: get_provider_env_value(KIMI_CODE_OAUTH_HOST_ENV, None)
            .or_else(|| get_provider_env_value(KIMI_OAUTH_HOST_ENV, None)),
        env_region: get_provider_env_value(KIMI_CODE_REGION_ENV, None),
        ..Default::default()
    });
    if from_env.env.is_some() {
        return Ok(from_env);
    }
    if let Err(reason) = interaction.signal.throw_if_aborted() {
        anyhow::bail!("{reason}");
    }
    let answer = interaction.prompt(kimi_code_region_prompt()).await?;
    if let Err(reason) = interaction.signal.throw_if_aborted() {
        anyhow::bail!("{reason}");
    }
    match KimiCodeRegion::from_id(&answer) {
        Some(region) => Ok(endpoints_for_region(region)),
        None => anyhow::bail!("Unknown Kimi Code region: {answer}"),
    }
}

pub fn kimi_code_region_env_value(region: KimiCodeRegion) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(KIMI_CODE_REGION_ENV.to_string(), region.id().to_string());
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAINLAND_OAUTH_HOST: &str = "https://auth.kimi.com";
    const GLOBAL_OAUTH_HOST: &str = "https://auth.kimi.ai";
    const GLOBAL_API_BASE_URL: &str = "https://api.kimi.ai/coding";

    fn env_of(entries: &[(&str, &str)]) -> Option<ProviderEnv> {
        let mut env = ProviderEnv::new();
        for (key, value) in entries {
            env.insert((*key).to_string(), (*value).to_string());
        }
        Some(env)
    }

    fn endpoints(region: Option<KimiCodeRegion>, oauth_host: &str, api_base_url: Option<&str>, env: Option<ProviderEnv>) -> KimiCodeEndpoints {
        KimiCodeEndpoints {
            region,
            oauth_host: oauth_host.to_string(),
            api_base_url: api_base_url.map(str::to_string),
            env,
        }
    }

    #[test]
    fn defaults_to_kimi_com_with_no_stored_or_env_facts_and_keeps_the_catalog_base() {
        assert_eq!(
            resolve_kimi_code_endpoints(KimiCodeEndpointInput::default()),
            endpoints(Some(KimiCodeRegion::MainlandCn), MAINLAND_OAUTH_HOST, None, None)
        );
    }

    #[test]
    fn moves_an_international_credential_to_kimi_ai_for_auth_and_inference() {
        let resolved = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            stored_region: Some("global".into()),
            ..Default::default()
        });
        assert_eq!(
            resolved,
            endpoints(
                Some(KimiCodeRegion::Global),
                GLOBAL_OAUTH_HOST,
                Some(GLOBAL_API_BASE_URL),
                env_of(&[(KIMI_CODE_REGION_ENV, "global")])
            )
        );
    }

    #[test]
    fn lets_the_stored_region_outrank_an_env_host_that_points_elsewhere() {
        let stored_global_env_mainland = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            stored_region: Some("global".into()),
            env_oauth_host: Some(MAINLAND_OAUTH_HOST.into()),
            ..Default::default()
        });
        assert_eq!(stored_global_env_mainland.oauth_host, GLOBAL_OAUTH_HOST);

        let stored_mainland_env_global = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            stored_region: Some("mainland-cn".into()),
            env_region: Some("global".into()),
            ..Default::default()
        });
        assert_eq!(stored_mainland_env_global.oauth_host, MAINLAND_OAUTH_HOST);
    }

    #[test]
    fn maps_a_known_env_host_onto_its_region_for_credentials_that_predate_regions() {
        let resolved = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            env_oauth_host: Some(format!("{GLOBAL_OAUTH_HOST}/")),
            ..Default::default()
        });
        assert_eq!(resolved.region, Some(KimiCodeRegion::Global));
        assert_eq!(resolved.api_base_url.as_deref(), Some(GLOBAL_API_BASE_URL));
    }

    #[test]
    fn keeps_a_custom_host_verbatim_without_a_region_or_an_inference_base() {
        let resolved = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            stored_oauth_host: Some("https://auth.example.com///".into()),
            ..Default::default()
        });
        assert_eq!(
            resolved,
            endpoints(None, "https://auth.example.com", None, env_of(&[(KIMI_CODE_OAUTH_HOST_ENV, "https://auth.example.com")]))
        );
    }

    #[test]
    fn prefers_an_explicit_env_host_over_an_env_region_and_ignores_unknown_region_values() {
        let host_wins = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            env_oauth_host: Some(MAINLAND_OAUTH_HOST.into()),
            env_region: Some("global".into()),
            ..Default::default()
        });
        assert_eq!(host_wins.region, Some(KimiCodeRegion::MainlandCn));

        let unknown_stored_falls_through = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            stored_region: Some("mars".into()),
            env_region: Some("global".into()),
            ..Default::default()
        });
        assert_eq!(unknown_stored_falls_through.region, Some(KimiCodeRegion::Global));

        let unknown_env = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            env_region: Some("mars".into()),
            ..Default::default()
        });
        assert_eq!(unknown_env.region, Some(KimiCodeRegion::MainlandCn));
    }

    #[test]
    fn recognizes_both_official_hosts_regardless_of_trailing_slashes() {
        assert_eq!(kimi_code_region_for_oauth_host(&format!("{MAINLAND_OAUTH_HOST}/")), Some(KimiCodeRegion::MainlandCn));
        assert_eq!(kimi_code_region_for_oauth_host(GLOBAL_OAUTH_HOST), Some(KimiCodeRegion::Global));
        assert_eq!(kimi_code_region_for_oauth_host("https://auth.example.com"), None);
    }

    #[test]
    fn offers_exactly_the_two_regions_the_official_client_knows() {
        let prompt = kimi_code_region_prompt();
        let AuthPromptKind::Select { options, .. } = prompt.kind else {
            panic!("expected a select prompt");
        };
        let ids: Vec<&str> = options.iter().map(|option| option.id.as_str()).collect();
        assert_eq!(ids, vec!["mainland-cn", "global"]);
    }
}
