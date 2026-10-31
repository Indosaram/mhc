//! Port of senpi `websearch/websearch/search-model.ts` (pin 903b8336).
//!
//! deviation: upstream reads `cost`/`compat` off a widened `NativeModelInfo`; the native
//! `NativeModelInfo` carries only the route fields, so this module takes the full catalog
//! `Model` and projects it with `native::model_info` for the route checks.
use super::native::{model_info, native_mapping, native_route_key};
use super::provider_endpoints::SearchProvider;
use maho_ext_api::Model;

pub const SESSION_SEARCH_MODEL: &str = "session";

/// Cheaper same-provider models for hosted search; used only when listed on the session's own
/// route and priced lower.
fn default_search_models(provider: SearchProvider) -> &'static [&'static str] {
    match provider {
        SearchProvider::Anthropic => &["claude-haiku-4-5", "claude-haiku-4.5"],
        SearchProvider::Openai => &["gpt-5.6-luna"],
        SearchProvider::Xai => &["grok-4.3"],
        SearchProvider::Deepseek => &["deepseek-v4-flash"],
        _ => &[],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchModelSource {
    Session,
    Setting,
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSearchModelChoice {
    pub model: String,
    pub fallback_model: Option<String>,
    pub source: SearchModelSource,
    pub warning: Option<String>,
}

impl NativeSearchModelChoice {
    fn session(model: &str) -> Self {
        Self {
            model: model.into(),
            fallback_model: None,
            source: SearchModelSource::Session,
            warning: None,
        }
    }
}

fn is_cheaper(candidate: &Model, session: &Model) -> bool {
    let (input, output) = (candidate.cost.input, candidate.cost.output);
    let not_pricier = input <= session.cost.input && output <= session.cost.output;
    not_pricier && (input < session.cost.input || output < session.cost.output)
}

fn same_route_models<'a>(session: &Model, available: &'a [Model]) -> Vec<&'a Model> {
    let route_key = native_route_key(&model_info(session));
    available
        .iter()
        .filter(|candidate| {
            candidate.provider == session.provider
                && native_route_key(&model_info(candidate)) == route_key
        })
        .collect()
}

fn choose(model: &str, session: &Model, source: SearchModelSource) -> NativeSearchModelChoice {
    NativeSearchModelChoice {
        model: model.into(),
        fallback_model: (model != session.id).then(|| session.id.clone()),
        source,
        warning: None,
    }
}

fn default_choice(session: &Model, candidates: &[&Model]) -> NativeSearchModelChoice {
    if let Some(mapping) = native_mapping(&model_info(session)) {
        for id in default_search_models(mapping.provider) {
            if let Some(candidate) = candidates.iter().find(|model| model.id == *id)
                && candidate.id != session.id
                && is_cheaper(candidate, session)
            {
                return choose(&candidate.id, session, SearchModelSource::Default);
            }
        }
    }
    NativeSearchModelChoice::session(&session.id)
}

/// Picks the model a native (hosted) search runs on for the session's own route. The choice always
/// stays on the session's provider, endpoint and credential; `fallback_model` is the session model.
/// `None` when the session model has no hosted search route.
pub fn resolve_native_search_model(
    session: Option<&Model>,
    available: &[Model],
    setting: Option<&str>,
) -> Option<NativeSearchModelChoice> {
    let session = session?;
    native_route_key(&model_info(session))?;
    // Main-request server search is always the active model's job (senpi#3122).
    if crate::server_search::has_server_search(Some(session)) {
        return Some(NativeSearchModelChoice::session(&session.id));
    }
    if setting == Some(SESSION_SEARCH_MODEL) {
        return Some(NativeSearchModelChoice::session(&session.id));
    }
    let candidates = same_route_models(session, available);
    let Some(setting) = setting else {
        return Some(default_choice(session, &candidates));
    };
    let chosen = candidates
        .iter()
        .find(|model| model.id == setting)
        .or_else(|| {
            candidates
                .iter()
                .find(|model| format!("{}/{}", model.provider, model.id) == setting)
        });
    if let Some(chosen) = chosen {
        return Some(choose(&chosen.id, session, SearchModelSource::Setting));
    }
    Some(NativeSearchModelChoice {
        model: session.id.clone(),
        fallback_model: None,
        source: SearchModelSource::Session,
        warning: Some(format!(
            "nativeModel \"{setting}\" is ignored: it is not a hosted-search model on the session's {} route, so native search uses {}.",
            session.provider, session.id
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(
        api: &str,
        provider: &str,
        id: &str,
        base: &str,
        cost: (f64, f64),
        supports: Option<bool>,
    ) -> Model {
        let mut value = json!({"id":id,"name":id,"api":api,"provider":provider,"baseUrl":base,"reasoning":false,"input":["text"],"cost":{"input":cost.0,"output":cost.1,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100});
        if let Some(supports) = supports {
            value["compat"] = json!({"supportsWebSearch": supports});
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn never_defaults_an_eligible_main_request_route_to_a_cheaper_model() {
        let route = model(
            "anthropic-messages",
            "anthropic",
            "claude-opus-5-5",
            "https://proxy.example/anthropic",
            (15., 75.),
            Some(true),
        );
        let cheaper = model(
            "anthropic-messages",
            "anthropic",
            "claude-haiku-4-5",
            "https://proxy.example/anthropic",
            (1., 5.),
            Some(true),
        );
        let available = [route.clone(), cheaper];
        let session = NativeSearchModelChoice::session("claude-opus-5-5");
        assert_eq!(
            resolve_native_search_model(Some(&route), &available, None),
            Some(session.clone())
        );
        assert_eq!(
            resolve_native_search_model(Some(&route), &available, Some("claude-haiku-4-5")),
            Some(session)
        );
    }

    #[test]
    fn defaults_a_no_server_route_to_a_cheaper_same_route_model() {
        let route = model(
            "anthropic-messages",
            "anthropic",
            "claude-opus-5-5",
            "https://proxy.example/anthropic",
            (15., 75.),
            Some(false),
        );
        let cheaper = model(
            "anthropic-messages",
            "anthropic",
            "claude-haiku-4-5",
            "https://proxy.example/anthropic",
            (1., 5.),
            Some(false),
        );
        let elsewhere = model(
            "anthropic-messages",
            "anthropic",
            "claude-haiku-4.5",
            "https://other.example/anthropic",
            (1., 5.),
            Some(false),
        );
        let choice =
            resolve_native_search_model(Some(&route), &[route.clone(), cheaper, elsewhere], None)
                .unwrap();
        assert_eq!(choice.model, "claude-haiku-4-5");
        assert_eq!(choice.fallback_model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(choice.source, SearchModelSource::Default);
    }

    #[test]
    fn settings_select_by_id_or_provider_path_and_warn_otherwise() {
        let route = model(
            "anthropic-messages",
            "anthropic",
            "claude-opus-5-5",
            "https://proxy.example/anthropic",
            (15., 75.),
            Some(false),
        );
        let other = model(
            "anthropic-messages",
            "anthropic",
            "claude-sonnet-5",
            "https://proxy.example/anthropic",
            (3., 15.),
            Some(false),
        );
        let available = [route.clone(), other];
        assert_eq!(
            resolve_native_search_model(Some(&route), &available, Some("session"))
                .unwrap()
                .source,
            SearchModelSource::Session
        );
        let by_path = resolve_native_search_model(
            Some(&route),
            &available,
            Some("anthropic/claude-sonnet-5"),
        )
        .unwrap();
        assert_eq!(
            (by_path.model.as_str(), by_path.source),
            ("claude-sonnet-5", SearchModelSource::Setting)
        );
        let unknown = resolve_native_search_model(Some(&route), &available, Some("gpt-5")).unwrap();
        assert_eq!(unknown.model, "claude-opus-5-5");
        assert!(
            unknown
                .warning
                .unwrap()
                .starts_with("nativeModel \"gpt-5\" is ignored")
        );
        assert_eq!(resolve_native_search_model(None, &available, None), None);
        let unmapped = model(
            "openai-completions",
            "local",
            "llama",
            "https://l.example",
            (0., 0.),
            None,
        );
        assert_eq!(
            resolve_native_search_model(Some(&unmapped), &[], None),
            None
        );
    }
}
