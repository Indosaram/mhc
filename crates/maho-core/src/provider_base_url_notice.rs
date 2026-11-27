//! Port of senpi packages/coding-agent/src/core/provider-base-url-notice.ts.

use indexmap::IndexMap;

use crate::model_config_schema::ModelsJsonProvider;

const FIXED_ADDRESS_PROVIDERS: [(&str, &str); 1] = [("cursor-cli-oauth", "Cursor")];

fn sets_base_url(config: &ModelsJsonProvider) -> bool {
    config.base_url.is_some()
        || config
            .models
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .any(|model| model.base_url.is_some())
}

/// One notice per fixed-address provider whose models.json entry sets a `baseUrl` (`provider` or
/// `models[i].baseUrl`): the setting is kept, but requests still go to the provider's own service.
pub fn fixed_address_base_url_notices(
    providers: &IndexMap<String, ModelsJsonProvider>,
) -> Vec<String> {
    let mut notices = Vec::new();
    for (provider_id, service) in FIXED_ADDRESS_PROVIDERS {
        let Some(config) = providers.get(provider_id) else {
            continue;
        };
        if !sets_base_url(config) {
            continue;
        }
        notices.push(format!(
            "models.json sets a baseUrl for {provider_id}, but {service} does not support changing the address. Requests still go to {service}."
        ));
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_notice_when_fixed_address_provider_sets_base_url() {
        let mut providers = IndexMap::new();
        providers.insert(
            "cursor-cli-oauth".to_owned(),
            ModelsJsonProvider {
                base_url: Some("https://proxy.example".to_owned()),
                ..Default::default()
            },
        );
        let notices = fixed_address_base_url_notices(&providers);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("cursor-cli-oauth"));
        assert!(notices[0].contains("Cursor"));
    }
}
