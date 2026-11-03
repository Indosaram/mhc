//! Port of senpi packages/coding-agent/src/core/provider-origin.ts.
//!
//! Where a provider's definition comes from, so a client can say where to change or remove an entry the
//! app cannot delete itself (senpi#4328).

/// - `Builtin`: shipped with maho; `models_json_path`/`extension_path` name an overlay when one exists.
/// - `Extension`: registered by an extension (natively, or as a provider config with no builtin).
/// - `ModelsJson`: defined only in models.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOriginKind {
    Builtin,
    ModelsJson,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOrigin {
    pub kind: ProviderOriginKind,
    /// The models.json that defines or overrides the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_json_path: Option<String>,
    /// Filesystem path of the registering extension, omitted for synthetic identifiers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderOriginFacts<'a> {
    pub builtin: bool,
    pub native_extension: bool,
    /// A provider-config registration or a virtual model from an extension.
    pub extension: bool,
    pub extension_path: Option<&'a str>,
    pub models_json_path: Option<&'a str>,
}

pub fn provider_origin_of(facts: ProviderOriginFacts<'_>) -> Option<ProviderOrigin> {
    let builtin_extension = facts
        .extension_path
        .is_some_and(|path| path.starts_with("<builtin:"));
    let extension_path = facts.extension_path.filter(|path| !path.starts_with('<'));
    let kind = if builtin_extension {
        ProviderOriginKind::Builtin
    } else if facts.native_extension {
        ProviderOriginKind::Extension
    } else if facts.builtin {
        ProviderOriginKind::Builtin
    } else if facts.extension {
        ProviderOriginKind::Extension
    } else if facts.models_json_path.is_some() {
        ProviderOriginKind::ModelsJson
    } else {
        return None;
    };
    Some(ProviderOrigin {
        kind,
        models_json_path: facts.models_json_path.map(str::to_owned),
        extension_path: extension_path.map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// senpi test/suite/regressions/4328-provider-origin.test.ts.
    #[test]
    fn origin_kinds_follow_registration_facts() {
        let builtin_ext = provider_origin_of(ProviderOriginFacts {
            native_extension: true,
            extension_path: Some("<builtin:vertex>"),
            ..Default::default()
        })
        .expect("origin");
        assert_eq!(builtin_ext.kind, ProviderOriginKind::Builtin);
        assert_eq!(builtin_ext.extension_path, None);
        let models_json = provider_origin_of(ProviderOriginFacts {
            models_json_path: Some("/a/models.json"),
            ..Default::default()
        })
        .expect("origin");
        assert_eq!(models_json.kind, ProviderOriginKind::ModelsJson);
        let overlay = provider_origin_of(ProviderOriginFacts {
            builtin: true,
            extension: true,
            extension_path: Some("/ext/index.ts"),
            ..Default::default()
        })
        .expect("origin");
        assert_eq!(overlay.kind, ProviderOriginKind::Builtin);
        assert_eq!(overlay.extension_path.as_deref(), Some("/ext/index.ts"));
        assert!(provider_origin_of(ProviderOriginFacts::default()).is_none());
    }
}
