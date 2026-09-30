//! Port of senpi packages/ai/src/api/cloudflare-ai-binding.ts.
//!
//! pi's Cloudflare AI Gateway support speaks HTTPS (`gateway.ai.cloudflare.com/v1/{account}/...`,
//! see `api/cloudflare.ts`), which needs a Cloudflare API token even for a Worker inside the
//! gateway's own account. A Worker avoids that token through the AI binding's `fetch` passthrough.
//! The Rust host has no `env.AI`, so this module ports the binding contract (the `AiBinding` shape,
//! the runtime `fetch` check and the sentinel placeholder) and leaves the request passthrough to a
//! caller-supplied implementation.

use std::sync::Arc;

/// The Workers AI binding (`env.AI`), described structurally so this module does not depend on a
/// concrete binding type. `fetch` is optional because the published type does not declare it yet;
/// `ai_gateway_log_id` pins the type to the AI binding.
pub trait AiBinding: Send + Sync {
    fn ai_gateway_log_id(&self) -> Option<String>;
    fn has_fetch(&self) -> bool;
    fn fetch(&self, input: &str, init: Option<&BindingRequestInit>) -> Result<BindingResponse, String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingRequestInit {
    pub method: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Placeholder value for auth headers on binding-routed requests. API implementations require an
/// API key or a recognized auth header (`authorization`, `x-api-key`, `cf-aig-authorization`)
/// before dispatch; binding calls are pre-authenticated, so pass
/// `cf-aig-authorization: Bearer ${CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}` to satisfy the check.
/// The gateway ignores (and strips) `cf-aig-authorization` on binding-routed requests. Pair it with
/// `Authorization: null` / `x-api-key: null` so the placeholder auth headers never reach the
/// gateway, which would treat a request-supplied auth header as a BYOK provider key.
pub const CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL: &str = "cloudflare-gateway-binding";

pub struct AiBindingFetch {
    binding: Arc<dyn AiBinding>,
}

impl AiBindingFetch {
    /// Create a fetch backed by the AI binding, for models whose `baseUrl` already names a route
    /// the binding serves. Requests pass through untouched.
    pub fn new(binding: Arc<dyn AiBinding>) -> Result<Self, String> {
        if !binding.has_fetch() {
            return Err("createAiBindingFetch: the AI binding does not expose fetch()".into());
        }
        Ok(Self { binding })
    }

    pub fn fetch(&self, input: &str, init: Option<&BindingRequestInit>) -> Result<BindingResponse, String> {
        self.binding.fetch(input, init)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MissingFetch;

    impl AiBinding for MissingFetch {
        fn ai_gateway_log_id(&self) -> Option<String> {
            None
        }

        fn has_fetch(&self) -> bool {
            false
        }

        fn fetch(&self, _input: &str, _init: Option<&BindingRequestInit>) -> Result<BindingResponse, String> {
            Err("unreachable".into())
        }
    }

    struct EchoFetch;

    impl AiBinding for EchoFetch {
        fn ai_gateway_log_id(&self) -> Option<String> {
            Some("log".into())
        }

        fn has_fetch(&self) -> bool {
            true
        }

        fn fetch(&self, input: &str, init: Option<&BindingRequestInit>) -> Result<BindingResponse, String> {
            Ok(BindingResponse {
                status: 200,
                headers: vec![("x-echo".into(), input.to_owned())],
                body: init.and_then(|init| init.body.clone()).unwrap_or_default(),
            })
        }
    }

    #[test]
    fn a_binding_without_fetch_is_rejected_at_construction() {
        assert_eq!(
            AiBindingFetch::new(Arc::new(MissingFetch)).err(),
            Some("createAiBindingFetch: the AI binding does not expose fetch()".into())
        );
    }

    #[test]
    fn requests_pass_through_untouched() {
        let fetch = AiBindingFetch::new(Arc::new(EchoFetch)).expect("fetch");
        let init = BindingRequestInit {
            method: Some("POST".into()),
            headers: vec![("cf-aig-authorization".into(), format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}"))],
            body: Some(b"{}".to_vec()),
        };
        let response = fetch.fetch("https://workers-binding.ai/ai-gateway/gateways/g/openai", Some(&init)).expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{}");
    }
}
