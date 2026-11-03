//! Port of senpi packages/coding-agent/src/core/provider-header-auth.ts.

use std::collections::BTreeMap;

use maho_ai::auth::headers::{has_credential_headers, is_credential_header_name};
use maho_ai::auth::types::{AuthCheck, AuthContext, AuthType};
use maho_ai::types::ProviderHeaders;

use crate::config_reference_resolver::{
    ConfigReferenceContext, ConfigValue, assert_config_reference_authority,
};
use crate::resolve_config_value::{
    get_config_value_env_var_names_of, is_command_config_value_of, is_config_value_configured_of,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderAuthStatusSource {
    ExtensionHeaders,
    ModelsJsonHeaders,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfiguredHeaderAuthStatus {
    Configured {
        source: String,
        label: Option<String>,
    },
    NotConfigured,
}

struct EffectiveHeader {
    name: String,
    value: ConfigValue,
    source: HeaderAuthStatusSource,
}

fn effective_headers(
    config_headers: Option<&BTreeMap<String, ConfigValue>>,
    extension_headers: Option<&BTreeMap<String, ConfigValue>>,
) -> BTreeMap<String, EffectiveHeader> {
    let mut effective = BTreeMap::new();
    add_headers(
        &mut effective,
        config_headers,
        HeaderAuthStatusSource::ModelsJsonHeaders,
    );
    add_headers(
        &mut effective,
        extension_headers,
        HeaderAuthStatusSource::ExtensionHeaders,
    );
    effective
}

/// senpi `addHeaders`: an extension header carrying a reference is refused as `forbidden`, and a models.json
/// reference needs its remembered trusted origin (senpi#3858). A refused header is not counted as configured.
fn add_headers(
    target: &mut BTreeMap<String, EffectiveHeader>,
    headers: Option<&BTreeMap<String, ConfigValue>>,
    source: HeaderAuthStatusSource,
) {
    if let Some(headers) = headers {
        for (name, value) in headers {
            let extension = (source == HeaderAuthStatusSource::ExtensionHeaders)
                .then(|| ConfigReferenceContext::extension(format!("extension.headers.{name}")));
            if assert_config_reference_authority(value, extension.as_ref()).is_err()
                || assert_config_reference_authority(value, None).is_err()
            {
                continue;
            }
            target.insert(
                name.to_lowercase(),
                EffectiveHeader {
                    name: name.clone(),
                    value: value.clone(),
                    source,
                },
            );
        }
    }
}

fn single_header(name: &str, value: &str) -> ProviderHeaders {
    let mut headers = ProviderHeaders::new();
    headers.insert(name.to_owned(), Some(value.to_owned()));
    headers
}

/// senpi `isConfiguredCredentialHeader`: a reference value on a credential header counts as configured.
fn is_configured_credential_header(header: &EffectiveHeader) -> bool {
    is_credential_header_name(&header.name)
        && match &header.value {
            ConfigValue::String(value) => {
                has_credential_headers(Some(&single_header(&header.name, value)))
            }
            ConfigValue::Reference { .. } => true,
        }
}

fn source_label(source: HeaderAuthStatusSource) -> &'static str {
    match source {
        HeaderAuthStatusSource::ExtensionHeaders => "extension_headers",
        HeaderAuthStatusSource::ModelsJsonHeaders => "models_json_headers",
    }
}

pub fn configured_header_auth_status(
    config_headers: Option<&BTreeMap<String, ConfigValue>>,
    extension_headers: Option<&BTreeMap<String, ConfigValue>>,
    env: Option<&std::collections::HashMap<String, String>>,
) -> Option<ConfiguredHeaderAuthStatus> {
    let mut saw_credential_header = false;
    for header in effective_headers(config_headers, extension_headers).values() {
        if !is_credential_header_name(&header.name) {
            continue;
        }
        saw_credential_header = true;
        if is_command_config_value_of(&header.value) {
            return Some(ConfiguredHeaderAuthStatus::Configured {
                source: "models_json_command".to_owned(),
                label: None,
            });
        }
        let env_names = get_config_value_env_var_names_of(&header.value);
        if header.value.as_str().is_some() && !env_names.is_empty() {
            if is_config_value_configured_of(&header.value, env) {
                return Some(ConfiguredHeaderAuthStatus::Configured {
                    source: "environment".to_owned(),
                    label: Some(env_names.join(", ")),
                });
            }
            continue;
        }
        if is_configured_credential_header(header) {
            return Some(ConfiguredHeaderAuthStatus::Configured {
                source: source_label(header.source).to_owned(),
                label: None,
            });
        }
    }
    if saw_credential_header {
        Some(ConfiguredHeaderAuthStatus::NotConfigured)
    } else {
        None
    }
}

pub fn header_auth_resolution_source(
    config_headers: Option<&BTreeMap<String, ConfigValue>>,
    extension_headers: Option<&BTreeMap<String, ConfigValue>>,
) -> Option<String> {
    let headers = effective_headers(config_headers, extension_headers);
    for header in headers.values() {
        if !is_configured_credential_header(header) {
            continue;
        }
        return Some(match header.source {
            HeaderAuthStatusSource::ExtensionHeaders => "provider extension headers".to_owned(),
            HeaderAuthStatusSource::ModelsJsonHeaders => "models.json headers".to_owned(),
        });
    }
    None
}

pub async fn check_configured_header_auth(
    headers: Option<&BTreeMap<String, ConfigValue>>,
    ctx: &dyn AuthContext,
    source: Option<&str>,
) -> Option<AuthCheck> {
    let headers = headers?;
    for header in effective_headers(Some(headers), None).values() {
        if !is_credential_header_name(&header.name) {
            continue;
        }
        if header.value.is_reference() || is_command_config_value_of(&header.value) {
            return Some(AuthCheck {
                ambient: false,
                source: source.map(str::to_owned),
                auth_type: AuthType::ApiKey,
            });
        }
        let env_names = get_config_value_env_var_names_of(&header.value);
        let mut configured = true;
        for env_name in &env_names {
            if ctx.env(env_name).await.is_none() {
                configured = false;
                break;
            }
        }
        if configured && is_configured_credential_header(header) {
            return Some(AuthCheck {
                ambient: false,
                source: source.map(str::to_owned),
                auth_type: AuthType::ApiKey,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(entries: &[(&str, &str)]) -> BTreeMap<String, ConfigValue> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), ConfigValue::from(*value)))
            .collect()
    }

    /// senpi test/suite/regressions/3858-reference-capability.test.ts: a trusted reference header is a configured
    /// credential; the same reference from an extension (or without an origin) is not.
    #[test]
    fn reference_headers_count_only_with_trusted_origin() {
        let mut trusted: ConfigValue =
            serde_json::from_value(serde_json::json!({"$env": "TOKEN"})).expect("reference");
        crate::config_reference_resolver::remember_config_reference_origin(
            &mut trusted,
            ConfigReferenceContext::trusted("/m.json", "providers.p.headers.Authorization"),
        );
        let config = BTreeMap::from([("Authorization".to_owned(), trusted.clone())]);
        assert_eq!(
            header_auth_resolution_source(Some(&config), None).as_deref(),
            Some("models.json headers")
        );
        let extension = BTreeMap::from([("Authorization".to_owned(), trusted)]);
        assert_eq!(header_auth_resolution_source(None, Some(&extension)), None);
        let bare: ConfigValue =
            serde_json::from_value(serde_json::json!({"$env": "TOKEN"})).expect("reference");
        let config = BTreeMap::from([("Authorization".to_owned(), bare)]);
        assert_eq!(header_auth_resolution_source(Some(&config), None), None);
    }

    #[test]
    fn no_credential_header_yields_none() {
        let config = headers(&[("X-Other", "value")]);
        assert!(configured_header_auth_status(Some(&config), None, None).is_none());
    }

    #[test]
    fn a_literal_credential_header_is_configured() {
        let config = headers(&[("Authorization", "Bearer abc")]);
        let status = configured_header_auth_status(Some(&config), None, None).expect("status");
        assert_eq!(
            status,
            ConfiguredHeaderAuthStatus::Configured {
                source: "models_json_headers".to_owned(),
                label: None
            }
        );
    }

    #[test]
    fn a_command_header_is_configured_from_models_json() {
        let config = headers(&[("Authorization", "!echo token")]);
        let status = configured_header_auth_status(Some(&config), None, None).expect("status");
        assert_eq!(
            status,
            ConfiguredHeaderAuthStatus::Configured {
                source: "models_json_command".to_owned(),
                label: None
            }
        );
    }

    #[test]
    fn a_missing_env_reports_not_configured() {
        let config = headers(&[("Authorization", "Bearer ${MISSING_KEY}")]);
        let status = configured_header_auth_status(
            Some(&config),
            None,
            Some(&std::collections::HashMap::new()),
        )
        .expect("status");
        assert_eq!(status, ConfiguredHeaderAuthStatus::NotConfigured);
    }

    #[test]
    fn the_resolution_source_names_the_origin() {
        let config = headers(&[("Authorization", "Bearer abc")]);
        assert_eq!(
            header_auth_resolution_source(Some(&config), None).as_deref(),
            Some("models.json headers")
        );
    }

    struct Env(std::collections::HashMap<String, String>);

    #[async_trait::async_trait]
    impl AuthContext for Env {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn a_literal_header_passes_the_async_check() {
        let config = headers(&[("Authorization", "Bearer abc")]);
        let ctx = Env(std::collections::HashMap::new());
        let check = check_configured_header_auth(Some(&config), &ctx, Some("models.json headers"))
            .await
            .expect("check");
        assert_eq!(check.auth_type, AuthType::ApiKey);
        assert_eq!(check.source.as_deref(), Some("models.json headers"));
    }

    #[tokio::test]
    async fn a_missing_env_fails_the_async_check() {
        let config = headers(&[("Authorization", "Bearer ${MISSING_KEY}")]);
        let ctx = Env(std::collections::HashMap::new());
        assert!(
            check_configured_header_auth(Some(&config), &ctx, None)
                .await
                .is_none()
        );
    }
}
