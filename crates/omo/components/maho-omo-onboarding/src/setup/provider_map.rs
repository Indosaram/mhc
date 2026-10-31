//! `provider-map.json` (embedded verbatim from omo v5.1.52 `omo-native/bin/lib`): the engine's
//! builtin and OAuth provider ids, and the source-harness ids mapped onto the omo provider that
//! serves the same endpoint (`providers`) or that a `/login` targets (`oauthLogins`).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMap {
    pub builtin_provider_ids: Vec<String>,
    pub oauth_provider_ids: Vec<String>,
    pub providers: BTreeMap<String, String>,
    pub oauth_logins: BTreeMap<String, String>,
}

static MAP: LazyLock<ProviderMap> = LazyLock::new(|| {
    serde_json::from_str(include_str!("provider-map.json")).unwrap_or_else(|_| ProviderMap {
        builtin_provider_ids: Vec::new(),
        oauth_provider_ids: Vec::new(),
        providers: BTreeMap::new(),
        oauth_logins: BTreeMap::new(),
    })
});

pub fn provider_map() -> &'static ProviderMap {
    &MAP
}

impl ProviderMap {
    pub fn is_builtin(&self, id: &str) -> bool {
        self.builtin_provider_ids.iter().any(|b| b == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream `setup-guidance.test.ts` "omo setup provider map".
    #[test]
    fn every_id_a_builtin_provider_serves_resolves_to_it() {
        let map = provider_map();
        assert!(!map.builtin_provider_ids.is_empty());
        let resolve = |id: &str| {
            if map.is_builtin(id) {
                Some(id.to_owned())
            } else {
                map.providers.get(id).cloned()
            }
        };
        assert_eq!(resolve("opencode").as_deref(), Some("opencode"));
        assert_eq!(resolve("opencode-go").as_deref(), Some("opencode-go"));
        assert_eq!(resolve("zai-coding-plan").as_deref(), Some("zai"));
        assert_eq!(resolve("kimi-for-coding").as_deref(), Some("kimi-coding"));
        assert_eq!(resolve("unknown-gateway"), None);
    }
}
