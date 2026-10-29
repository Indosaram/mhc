use std::time::Duration;
use axum::{Form, Json, Router, extract::State, routing::{get, post}};
use maho_ext_mcp::auth::{
    oauth::{add_resource_param, begin_authorization, client_credentials_grant, finish_authorization, normalize_resource_url},
    oauth_provider::McpOAuthProvider,
    oauth_refresh::McpRefreshManager,
    token_store::{McpStoredAuth, McpTokenStore},
};
use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

type FormPairs = Vec<(String, String)>;

struct Fixture {
    base: String,
    forms: UnboundedReceiver<FormPairs>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn token(State(sender): State<UnboundedSender<FormPairs>>, Form(form): Form<FormPairs>) -> Json<serde_json::Value> {
    sender.send(form).expect("test fixture");
    Json(json!({"access_token":"fixture-new","refresh_token":"fixture-next","token_type":"Bearer","expires_in":3600}))
}

async fn fixture() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let base = format!("http://{}", listener.local_addr().expect("test fixture"));
    let (sender, forms) = unbounded_channel();
    let metadata = {
        let base = base.clone();
        json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "code_challenge_methods_supported": ["S256"],
        })
    };
    let app = Router::new()
        .route("/.well-known/oauth-authorization-server", get(move || {
            let metadata = metadata.clone();
            async move { Json(metadata) }
        }))
        .route("/token", post(token))
        .with_state(sender);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test fixture");
    });
    Fixture { base, forms, task }
}

fn provider(root: &std::path::Path, name: &str, url: &str) -> McpOAuthProvider {
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(root, name, url));
    provider.require_https = false;
    provider.client_id = Some("fixture-client".into());
    provider.redirect_url = Some("http://127.0.0.1:9/callback".into());
    provider
}

async fn next_form(fixture: &mut Fixture) -> FormPairs {
    tokio::time::timeout(Duration::from_secs(5), fixture.forms.recv()).await.expect("test fixture").expect("test fixture")
}

fn resources(form: &FormPairs) -> Vec<&str> {
    form.iter().filter(|(key, _)| key == "resource").map(|(_, value)| value.as_str()).collect()
}

#[test]
fn resource_normalization_table() {
    for (input, expected) in [
        ("https://a.test", "https://a.test"),
        ("https://a.test/mcp/?t=1#f", "https://a.test/mcp"),
        ("https://a.test/mcp//", "https://a.test/mcp/"),
        ("https://a.test/mcp", "https://a.test/mcp"),
        ("not a url", "not a url"),
    ] {
        assert_eq!(normalize_resource_url(input), expected, "{input}");
    }
}

fn pairs(items: &[(&str, &str)]) -> FormPairs {
    items.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn resource_param_replaces_first_dedups_or_appends() {
    let mut form = pairs(&[("a", "1"), ("resource", "old"), ("b", "2"), ("resource", "dup"), ("c", "3")]);
    add_resource_param(&mut form, "new");
    assert_eq!(form, pairs(&[("a", "1"), ("resource", "new"), ("b", "2"), ("c", "3")]));
    let mut form = pairs(&[("a", "1")]);
    add_resource_param(&mut form, "new");
    assert_eq!(form, pairs(&[("a", "1"), ("resource", "new")]));
}

#[tokio::test]
async fn resource_is_sent_on_authorize_exchange_refresh_and_client_credentials() {
    let mut fixture = fixture().await;
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let server_url = format!("{}/mcp/?t=1#f", fixture.base);
    let expected = format!("{}/mcp", fixture.base);

    let mut provider = provider(root.path(), "resource", &server_url);
    let begin = begin_authorization(&mut provider, &client).await.expect("test fixture");
    let authorization = begin.authorization_url.expect("test fixture");
    let sent: Vec<_> = authorization.query_pairs().filter(|(key, _)| key == "resource").collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, expected);

    finish_authorization(&provider, "fixture-code", &client).await.expect("test fixture");
    assert_eq!(resources(&next_form(&mut fixture).await), [expected.as_str()]);

    // Refresh: the stored resource wins over the computed one.
    let store = provider.store.clone();
    store
        .update(|record| {
            let mut record = record.unwrap_or_default();
            record.expires_at = Some(0.0);
            record.resource = Some("https://stored.test/resource".into());
            Some(record)
        })
        .expect("test fixture");
    let manager = McpRefreshManager::new(std::sync::Arc::new(self::provider(root.path(), "resource", &server_url)), client.clone());
    manager.force_refresh().await.expect("test fixture");
    let form = next_form(&mut fixture).await;
    assert!(form.contains(&("grant_type".into(), "refresh_token".into())));
    assert_eq!(resources(&form), ["https://stored.test/resource"]);

    // Refresh without a stored resource uses the computed one.
    store.write(McpStoredAuth { access_token: Some("a".into()), refresh_token: Some("r".into()), expires_at: Some(0.0), ..Default::default() }).expect("test fixture");
    manager.force_refresh().await.expect("test fixture");
    assert_eq!(resources(&next_form(&mut fixture).await), [expected.as_str()]);

    client_credentials_grant(&provider, &client).await.expect("test fixture");
    let form = next_form(&mut fixture).await;
    assert!(form.contains(&("grant_type".into(), "client_credentials".into())));
    assert_eq!(resources(&form), [expected.as_str()]);
}
