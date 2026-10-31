//! Shared fixtures for the senpi#3858 reference ports (upstream test/suite/regressions/3858-reference-support.ts).
//!
//! The resolved-value registry is process global and every test module shares one binary, so each test uses its
//! own distinct secret instead of upstream's shared `SECRET` / `ROTATED`.

use axum::{Json, Router, http::HeaderMap, response::IntoResponse, routing::post};
use maho_ai::utils::config_reference::ConfigReference;
use maho_core::config_reference_resolver::{
    ConfigReferenceContext, ConfigValue, resolve_config_reference,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// A trusted `$env` reference resolved through the real resolver, which remembers the value for redaction.
pub fn resolve_env(name: &str, value: &str) -> String {
    let reference = ConfigValue::Reference {
        reference: ConfigReference::Env(name.into()),
        origin: Some(Box::new(ConfigReferenceContext::trusted(
            "/fixture/models.json",
            "provider.apiKey",
        ))),
    };
    let env = HashMap::from([(name.to_owned(), value.to_owned())]);
    resolve_config_reference(&reference, None, Some(&env)).expect("trusted env reference")
}

/// JS `encodeURIComponent` for the ASCII fixtures used here.
pub fn encode_uri_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// A legacy-era streamable-HTTP MCP stub that records the `Authorization` header of every request.
pub struct HeaderStub {
    pub url: String,
    pub authorizations: Arc<Mutex<Vec<Option<String>>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl HeaderStub {
    pub async fn start() -> Self {
        let authorizations = Arc::new(Mutex::new(Vec::new()));
        let observed = authorizations.clone();
        let app = Router::new().route(
            "/mcp",
            post(move |headers: HeaderMap, Json(request): Json<Value>| {
                let observed = observed.clone();
                async move {
                    observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                    );
                    if request.get("id").is_none() {
                        return axum::http::StatusCode::ACCEPTED.into_response();
                    }
                    let method = request["method"].as_str().unwrap_or_default();
                    if method == "server/discover" {
                        return Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}})).into_response();
                    }
                    let result = match method {
                        "initialize" => {
                            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"reference","version":"1"}})
                        }
                        _ => json!({"tools":[],"resources":[],"prompts":[]}),
                    };
                    Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub listener");
        let address = listener.local_addr().expect("stub address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("stub server");
        });
        Self {
            url: format!("http://{address}/mcp"),
            authorizations,
            stop: Some(stop),
            task,
        }
    }

    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.authorizations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), self.task)
            .await
            .expect("stub stops in time")
            .expect("stub task");
    }
}
