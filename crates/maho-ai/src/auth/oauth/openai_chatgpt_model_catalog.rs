//! Port of senpi packages/ai/src/auth/oauth/openai-chatgpt-model-catalog.ts.
//!
//! The SIWC (sign-in with ChatGPT) account model catalog is an account-specific server contract:
//! `GET https://api.openai.com/v1/models` with the selected account's bearer answers the slugs that
//! account may pick, and supplies no capability metadata for unknown slugs. Catalogs are cached per
//! verified identity (subject + issued client ID, never the rotating bearer) with transactional
//! refresh, last-known retention on failure, and generation guards against out-of-order publishes.
//!
//! The HTTP call goes through the crate's existing [`OAuthTransport`] seam, so tests drive a
//! scripted transport and production uses the shared reqwest transport.

use crate::auth::oauth::transport::{HttpRequest, OAuthTransport, default_transport};
use crate::models::{ModelsError, ModelsErrorCode};
use crate::types::{InputModality, ModelCost, ThinkingLevelMap};
use crate::utils::abort::{AbortReason, AbortSignal};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

pub const SIWC_MODELS_URL: &str = "https://api.openai.com/v1/models";

/// Verified identity, not email, slot label, or a rotating bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAIChatGptModelIdentity {
    pub sub: String,
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAIChatGptModelAccount {
    pub sub: String,
    pub client_id: String,
    /// Supply the access token after the runtime's shared OAuth refresh has completed.
    pub access: String,
}

impl OpenAIChatGptModelAccount {
    pub fn identity(&self) -> OpenAIChatGptModelIdentity {
        OpenAIChatGptModelIdentity {
            sub: self.sub.clone(),
            client_id: self.client_id.clone(),
        }
    }
}

/// Picker entries are not full inference models: `/v1/models` supplies no capability metadata for
/// unknown slugs. The slot adapter must preserve that distinction, so every capability field stays
/// absent instead of defaulting to a zero/false value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIChatGptCatalogModel {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<InputModality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
}

/// The TS `OpenAIChatGptModelCatalogError extends ModelsError` carries `code: "model_source"` and an
/// optional HTTP status. senpi's aborted paths rethrow the signal's reason instead of building a
/// catalog error, so cancellation is the second variant rather than a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAIChatGptModelCatalogError {
    Catalog {
        error: ModelsError,
        status: Option<u16>,
    },
    Cancelled(AbortReason),
}

impl std::fmt::Display for OpenAIChatGptModelCatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog { error, .. } => f.write_str(&error.message),
            Self::Cancelled(reason) => f.write_str(&reason.message),
        }
    }
}

impl std::error::Error for OpenAIChatGptModelCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Catalog { error, .. } => Some(error),
            // The reason is already this variant's message; surfacing it again would double-print.
            Self::Cancelled(_) => None,
        }
    }
}

impl OpenAIChatGptModelCatalogError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Catalog {
            error: ModelsError::new(ModelsErrorCode::ModelSource, message),
            status: None,
        }
    }

    pub fn http_status(status: u16) -> Self {
        Self::Catalog {
            error: ModelsError::new(
                ModelsErrorCode::ModelSource,
                format!("SIWC model catalog request failed (HTTP {status})"),
            ),
            status: Some(status),
        }
    }

    pub fn from_abort(reason: &AbortReason) -> Self {
        Self::Cancelled(reason.clone())
    }

    /// `error.code` on the TS class; a rethrown abort reason has none.
    pub fn code(&self) -> Option<ModelsErrorCode> {
        match self {
            Self::Catalog { error, .. } => Some(error.code),
            Self::Cancelled(_) => None,
        }
    }

    /// `error.status` on the TS class.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Catalog { status, .. } => *status,
            Self::Cancelled(_) => None,
        }
    }
}

fn identity_key(identity: &OpenAIChatGptModelIdentity) -> String {
    // The TS `JSON.stringify([identity.sub, identity.clientId])`.
    Value::Array(vec![
        Value::String(identity.sub.clone()),
        Value::String(identity.client_id.clone()),
    ])
    .to_string()
}

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn parse_catalog(
    raw: &Value,
) -> Result<Vec<OpenAIChatGptCatalogModel>, OpenAIChatGptModelCatalogError> {
    let models = as_record(raw)
        .and_then(|record| record.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| OpenAIChatGptModelCatalogError::new("Invalid SIWC models response"))?;
    // Upstream reads `Object.values(CHATGPT_SUBSCRIPTION_MODELS)`; the embedded shard is the same data.
    let static_models = crate::models_generated::MODELS.get("chatgpt-subscription");
    let mut catalog = Vec::new();
    for item in models {
        let Some(item) = as_record(item) else {
            continue;
        };
        if item.get("visibility").and_then(Value::as_str) != Some("list") {
            continue;
        }
        let slug = item.get("slug").and_then(Value::as_str).unwrap_or_default();
        let display_name = item
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if slug.is_empty() || display_name.is_empty() {
            return Err(OpenAIChatGptModelCatalogError::new(
                "Invalid listed SIWC model",
            ));
        }
        let known = static_models.and_then(|models| models.values().find(|model| model.id == slug));
        catalog.push(OpenAIChatGptCatalogModel {
            id: slug.to_owned(),
            name: display_name.to_owned(),
            context_window: known.map(|model| model.context_window),
            max_tokens: known.map(|model| model.max_tokens),
            reasoning: known.map(|model| model.reasoning),
            thinking_level_map: known.and_then(|model| model.thinking_level_map.clone()),
            input: known.map(|model| model.input.clone()),
            cost: known.map(|model| model.cost.clone()),
        });
    }
    Ok(catalog)
}

#[derive(Default)]
struct CatalogState {
    catalogs: HashMap<String, Vec<OpenAIChatGptCatalogModel>>,
    generations: HashMap<String, u64>,
    active_key: Option<String>,
    active_needs_refresh: bool,
}

/// Last-known, account-scoped catalogs with transactional refresh, like provider `refreshModels`.
/// This owns neither OAuth refresh nor provider registration. Keep one instance per provider
/// runtime; activate the selected slot on account changes, or refresh after explicit model
/// refresh/token renewal.
pub struct OpenAIChatGptModelCatalog {
    transport: Arc<dyn OAuthTransport>,
    state: Mutex<CatalogState>,
}

impl OpenAIChatGptModelCatalog {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self {
            transport,
            state: Mutex::new(CatalogState {
                active_needs_refresh: true,
                ..CatalogState::default()
            }),
        }
    }

    /// One instance per provider runtime over the crate's shared transport.
    pub fn with_default_transport() -> Self {
        Self::new(default_transport())
    }

    fn state(&self) -> MutexGuard<'_, CatalogState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The active account's catalog, or `identity`'s. Unknown identities read as empty rather than
    /// borrowing a sibling account's list.
    pub fn get_models(
        &self,
        identity: Option<&OpenAIChatGptModelIdentity>,
    ) -> Vec<OpenAIChatGptCatalogModel> {
        let state = self.state();
        let key = match identity {
            Some(identity) => identity_key(identity),
            None => match state.active_key.clone() {
                Some(key) => key,
                None => return Vec::new(),
            },
        };
        state.catalogs.get(&key).cloned().unwrap_or_default()
    }

    pub async fn activate(
        &self,
        account: &OpenAIChatGptModelAccount,
        signal: &AbortSignal,
    ) -> Result<Vec<OpenAIChatGptCatalogModel>, OpenAIChatGptModelCatalogError> {
        let key = identity_key(&account.identity());
        {
            let mut state = self.state();
            if state.active_key.as_deref() != Some(key.as_str()) {
                state.active_key = Some(key.clone());
                state.active_needs_refresh = true;
            }
            if !state.active_needs_refresh {
                return Ok(state.catalogs.get(&key).cloned().unwrap_or_default());
            }
        }
        let models = self.refresh(account, signal).await?;
        let mut state = self.state();
        if state.active_key.as_deref() == Some(key.as_str()) {
            state.active_needs_refresh = false;
        }
        Ok(models)
    }

    pub async fn refresh(
        &self,
        account: &OpenAIChatGptModelAccount,
        signal: &AbortSignal,
    ) -> Result<Vec<OpenAIChatGptCatalogModel>, OpenAIChatGptModelCatalogError> {
        signal
            .throw_if_aborted()
            .map_err(|reason| OpenAIChatGptModelCatalogError::from_abort(&reason))?;
        let key = identity_key(&account.identity());
        let generation = {
            let mut state = self.state();
            let generation = state.generations.get(&key).copied().unwrap_or(0) + 1;
            state.generations.insert(key.clone(), generation);
            generation
        };
        let request = HttpRequest {
            method: "GET".to_owned(),
            url: SIWC_MODELS_URL.to_owned(),
            headers: vec![(
                "Authorization".to_owned(),
                format!("Bearer {}", account.access),
            )],
            body: None,
            timeout_ms: None,
        };
        let response =
            self.transport
                .execute(request, signal)
                .await
                .map_err(|error| match signal.reason() {
                    Some(reason) => OpenAIChatGptModelCatalogError::from_abort(&reason),
                    None => OpenAIChatGptModelCatalogError::new(error.to_string()),
                })?;
        if !response.ok() {
            return Err(OpenAIChatGptModelCatalogError::http_status(response.status));
        }
        let raw: Value = serde_json::from_str(&response.body)
            .map_err(|_| OpenAIChatGptModelCatalogError::new("Invalid SIWC models response"))?;
        let models = parse_catalog(&raw)?;
        // An endpoint that answers after the caller cancelled must not publish its list.
        signal
            .throw_if_aborted()
            .map_err(|reason| OpenAIChatGptModelCatalogError::from_abort(&reason))?;
        {
            let mut state = self.state();
            if state.generations.get(&key).copied() == Some(generation) {
                state.catalogs.insert(key, models.clone());
            }
        }
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::HttpResponse;
    use crate::utils::abort::AbortController;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::VecDeque;
    use tokio::sync::oneshot;

    fn account_a() -> OpenAIChatGptModelAccount {
        OpenAIChatGptModelAccount {
            sub: "subject-a".into(),
            client_id: "client-a".into(),
            access: "synthetic-a".into(),
        }
    }

    fn account_b() -> OpenAIChatGptModelAccount {
        OpenAIChatGptModelAccount {
            sub: "subject-b".into(),
            client_id: "client-b".into(),
            access: "synthetic-b".into(),
        }
    }

    fn listed(slug: &str) -> Value {
        json!({"slug": slug, "display_name": format!("Display {slug}"), "visibility": "list"})
    }

    fn body(models: Vec<Value>) -> Value {
        json!({ "models": models })
    }

    fn ids(models: &[OpenAIChatGptCatalogModel]) -> Vec<String> {
        models.iter().map(|model| model.id.clone()).collect()
    }

    enum Reply {
        Json(Value),
        Status(u16),
        /// Signals `started`, then answers only once the test releases the gate.
        Gated {
            started: Option<oneshot::Sender<()>>,
            release: oneshot::Receiver<Value>,
        },
        /// Aborts the caller inside the request, then answers anyway.
        AbortThenJson {
            controller: AbortController,
            body: Value,
        },
    }

    struct TestTransport {
        replies: Mutex<VecDeque<Reply>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl TestTransport {
        fn new(replies: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    fn json_response(body: &Value) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string(),
        }
    }

    #[async_trait]
    impl OAuthTransport for TestTransport {
        async fn execute(
            &self,
            request: HttpRequest,
            _signal: &AbortSignal,
        ) -> anyhow::Result<HttpResponse> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            let reply = self
                .replies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .expect("TestTransport has no scripted reply");
            match reply {
                Reply::Json(body) => Ok(json_response(&body)),
                Reply::Status(status) => Ok(HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: "untrusted response body".into(),
                }),
                Reply::Gated { started, release } => {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    let body = release
                        .await
                        .map_err(|_| anyhow::anyhow!("gate released without a body"))?;
                    Ok(json_response(&body))
                }
                Reply::AbortThenJson { controller, body } => {
                    controller.abort(Some(AbortReason::new("Error", "cancelled")));
                    Ok(json_response(&body))
                }
            }
        }
    }

    #[test]
    fn identity_key_is_the_json_pair_of_subject_and_issued_client_id() {
        // Matches the TS `JSON.stringify([sub, clientId])` byte for byte.
        assert_eq!(
            identity_key(&account_a().identity()),
            r#"["subject-a","client-a"]"#
        );
        assert_ne!(
            identity_key(&account_a().identity()),
            identity_key(&account_b().identity())
        );
    }

    #[tokio::test]
    async fn requests_the_inference_endpoint_with_the_selected_bearer_and_preserves_server_order() {
        let transport = TestTransport::new(vec![Reply::Json(body(vec![
            listed("z-new"),
            json!({"slug": "hidden", "display_name": "Display hidden", "visibility": "hidden"}),
            listed("a-new"),
            json!({"slug": "not-for-picker", "display_name": "Internal"}),
        ]))]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport.clone());
        let models = catalog.activate(&account_a(), &signal).await.unwrap();
        assert_eq!(ids(&models), vec!["z-new", "a-new"]);
        assert_eq!(models[0].name, "Display z-new");
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].url, "https://api.openai.com/v1/models");
        assert_eq!(
            requests[0].headers,
            vec![(
                "Authorization".to_string(),
                "Bearer synthetic-a".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn keeps_separate_caches_for_both_subject_and_issued_client_id_not_bearer_token() {
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("a")])),
            Reply::Json(body(vec![listed("b")])),
            Reply::Json(body(vec![listed("other-registration")])),
            Reply::Json(body(vec![listed("other-subject")])),
        ]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport);
        let (a, b) = (account_a(), account_b());
        catalog.activate(&a, &signal).await.unwrap();
        catalog.activate(&b, &signal).await.unwrap();
        let registration = OpenAIChatGptModelAccount {
            sub: a.sub.clone(),
            client_id: "client-c".into(),
            access: b.access.clone(),
        };
        catalog.activate(&registration, &signal).await.unwrap();
        let subject = OpenAIChatGptModelAccount {
            sub: "subject-c".into(),
            client_id: a.client_id.clone(),
            access: a.access.clone(),
        };
        catalog.activate(&subject, &signal).await.unwrap();
        assert_eq!(ids(&catalog.get_models(Some(&a.identity()))), vec!["a"]);
        assert_eq!(ids(&catalog.get_models(Some(&b.identity()))), vec!["b"]);
        assert_eq!(
            ids(&catalog.get_models(Some(&registration.identity()))),
            vec!["other-registration"]
        );
        assert_eq!(
            ids(&catalog.get_models(Some(&subject.identity()))),
            vec!["other-subject"]
        );
        let rotated = OpenAIChatGptModelAccount {
            access: "rotated-a".into(),
            ..a.clone()
        };
        assert_eq!(
            catalog.get_models(Some(&rotated.identity())),
            catalog.get_models(Some(&a.identity()))
        );
    }

    #[tokio::test]
    async fn refreshes_on_every_active_identity_switch_including_a_return_to_a_cached_account() {
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("old-a")])),
            Reply::Json(body(vec![listed("b")])),
            Reply::Json(body(vec![listed("new-a")])),
        ]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport.clone());
        let (a, b) = (account_a(), account_b());
        catalog.activate(&a, &signal).await.unwrap();
        let rotated = OpenAIChatGptModelAccount {
            access: "rotated-a".into(),
            ..a.clone()
        };
        catalog.activate(&rotated, &signal).await.unwrap();
        assert_eq!(
            transport.requests().len(),
            1,
            "a token renewal is not a switch"
        );
        catalog.activate(&b, &signal).await.unwrap();
        catalog.activate(&a, &signal).await.unwrap();
        assert_eq!(transport.requests().len(), 3);
        assert_eq!(ids(&catalog.get_models(None)), vec!["new-a"]);
    }

    #[tokio::test]
    async fn merges_matching_static_capabilities_without_inventing_metadata_for_unknown_slugs() {
        let known = crate::providers::chatgpt_subscription_models::chatgpt_subscription_models()
            .into_iter()
            .next()
            .expect("static ChatGPT subscription catalog is empty");
        let transport = TestTransport::new(vec![Reply::Json(body(vec![
            listed(&known.id),
            listed("unknown-future-model"),
        ]))]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport);
        let models = catalog.activate(&account_a(), &signal).await.unwrap();
        assert_eq!(models[0].id, known.id);
        assert_eq!(models[0].name, format!("Display {}", known.id));
        assert_eq!(models[0].context_window, Some(known.context_window));
        assert_eq!(models[0].max_tokens, Some(known.max_tokens));
        assert_eq!(models[0].reasoning, Some(known.reasoning));
        assert_eq!(models[0].thinking_level_map, known.thinking_level_map);
        assert_eq!(models[0].input, Some(known.input));
        assert_eq!(models[0].cost, Some(known.cost));
        assert_eq!(
            models[1],
            OpenAIChatGptCatalogModel {
                id: "unknown-future-model".into(),
                name: "Display unknown-future-model".into(),
                ..OpenAIChatGptCatalogModel::default()
            }
        );
    }

    #[tokio::test]
    async fn retains_the_previous_account_catalog_and_surfaces_a_typed_http_failure() {
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("previous")])),
            Reply::Status(503),
        ]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport);
        let account = account_a();
        catalog.activate(&account, &signal).await.unwrap();
        let error = catalog.refresh(&account, &signal).await.unwrap_err();
        assert_eq!(
            error.code().map(ModelsErrorCode::as_str),
            Some("model_source")
        );
        assert_eq!(error.status(), Some(503));
        assert_eq!(
            error.to_string(),
            "SIWC model catalog request failed (HTTP 503)"
        );
        assert!(matches!(
            &error,
            OpenAIChatGptModelCatalogError::Catalog { .. }
        ));
        assert_eq!(ids(&catalog.get_models(None)), vec!["previous"]);
    }

    #[tokio::test]
    async fn never_shows_a_sibling_cache_when_activation_fails_and_retries_the_failed_activation() {
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("a")])),
            Reply::Status(403),
            Reply::Json(body(vec![listed("b")])),
        ]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport);
        let (a, b) = (account_a(), account_b());
        catalog.activate(&a, &signal).await.unwrap();
        let error = catalog.activate(&b, &signal).await.unwrap_err();
        assert_eq!(error.status(), Some(403));
        assert_eq!(error.code(), Some(ModelsErrorCode::ModelSource));
        assert!(catalog.get_models(None).is_empty());
        let models = catalog.activate(&b, &signal).await.unwrap();
        assert_eq!(ids(&models), vec!["b"]);
    }

    #[tokio::test]
    async fn rejects_malformed_catalog_data_transactionally() {
        for malformed in [
            json!({"data": []}),
            json!({"models": [{"visibility": "list", "slug": 42}]}),
        ] {
            let transport = TestTransport::new(vec![
                Reply::Json(body(vec![listed("previous")])),
                Reply::Json(malformed.clone()),
            ]);
            let signal = AbortController::new().signal();
            let catalog = OpenAIChatGptModelCatalog::new(transport);
            let account = account_a();
            catalog.activate(&account, &signal).await.unwrap();
            let error = catalog.refresh(&account, &signal).await.unwrap_err();
            assert_eq!(error.status(), None, "{malformed}");
            assert_eq!(
                error.code(),
                Some(ModelsErrorCode::ModelSource),
                "{malformed}"
            );
            assert_eq!(
                ids(&catalog.get_models(None)),
                vec!["previous"],
                "{malformed}"
            );
        }
    }

    #[test]
    fn a_fresh_instance_and_unknown_identities_read_as_empty_rather_than_a_sibling_list() {
        let catalog = OpenAIChatGptModelCatalog::new(TestTransport::new(Vec::new()));
        assert!(catalog.get_models(None).is_empty());
        assert!(catalog.get_models(Some(&account_a().identity())).is_empty());
    }

    #[test]
    fn rejects_a_malformed_listed_entry_and_a_body_without_a_models_array() {
        let listed_without_name = json!({"models": [{"slug": "x", "visibility": "list"}]});
        assert_eq!(
            parse_catalog(&listed_without_name).unwrap_err().to_string(),
            "Invalid listed SIWC model"
        );
        let not_an_array = json!({"models": "nope"});
        assert_eq!(
            parse_catalog(&not_an_array).unwrap_err().to_string(),
            "Invalid SIWC models response"
        );
    }

    #[tokio::test]
    async fn publishes_an_empty_list_when_the_server_no_longer_lists_any_models() {
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("previous")])),
            Reply::Json(body(Vec::new())),
        ]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport);
        let account = account_a();
        catalog.activate(&account, &signal).await.unwrap();
        assert!(catalog.refresh(&account, &signal).await.unwrap().is_empty());
        assert!(catalog.get_models(None).is_empty());
    }

    #[tokio::test]
    async fn does_not_let_an_older_refresh_overwrite_a_newer_catalog_for_the_same_identity() {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("initial")])),
            Reply::Gated {
                started: Some(started_tx),
                release: release_rx,
            },
            Reply::Json(body(vec![listed("newer")])),
        ]);
        let signal = AbortController::new().signal();
        let catalog = Arc::new(OpenAIChatGptModelCatalog::new(transport));
        let account = account_a();
        catalog.activate(&account, &signal).await.unwrap();

        let older = tokio::spawn({
            let catalog = catalog.clone();
            let account = account.clone();
            let signal = signal.clone();
            async move { catalog.refresh(&account, &signal).await }
        });
        started_rx
            .await
            .expect("older refresh reached the endpoint");
        catalog.refresh(&account, &signal).await.unwrap();
        release_tx
            .send(body(vec![listed("older")]))
            .expect("older refresh still waiting");
        older.await.unwrap().unwrap();

        assert_eq!(ids(&catalog.get_models(None)), vec!["newer"]);
    }

    #[tokio::test]
    async fn does_not_publish_an_aborted_refresh_even_if_the_endpoint_completes() {
        let controller = AbortController::new();
        let transport = TestTransport::new(vec![
            Reply::Json(body(vec![listed("initial")])),
            Reply::AbortThenJson {
                controller: controller.clone(),
                body: body(vec![listed("aborted")]),
            },
        ]);
        let signal = AbortController::new().signal();
        let catalog = OpenAIChatGptModelCatalog::new(transport);
        let account = account_a();
        catalog.activate(&account, &signal).await.unwrap();
        let error = catalog
            .refresh(&account, &controller.signal())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "cancelled");
        assert!(matches!(
            &error,
            OpenAIChatGptModelCatalogError::Cancelled(_)
        ));
        assert_eq!(error.code(), None);
        assert_eq!(ids(&catalog.get_models(None)), vec!["initial"]);
    }

    #[tokio::test]
    async fn an_already_aborted_signal_never_reaches_the_endpoint() {
        let transport = TestTransport::new(Vec::new());
        let controller = AbortController::new();
        controller.abort(Some(AbortReason::new(
            "AbortError",
            "This operation was aborted",
        )));
        let catalog = OpenAIChatGptModelCatalog::new(transport.clone());
        let error = catalog
            .activate(&account_a(), &controller.signal())
            .await
            .unwrap_err();
        assert!(matches!(
            &error,
            OpenAIChatGptModelCatalogError::Cancelled(_)
        ));
        assert!(transport.requests().is_empty());
    }
}
