//! Port of senpi packages/ai/src/providers/cloudflare-stream.ts.

use crate::types::{Model, ProviderEnv, ProviderStreams, SimpleStreamOptions, StreamOptions};
use std::sync::Arc;

const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

pub fn resolve_cloudflare_model(model: &Model, env: Option<&ProviderEnv>) -> Model {
    let Some(env) = env else { return model.clone() };
    let account = env.get(CLOUDFLARE_ACCOUNT_ID).cloned();
    let gateway = env.get(CLOUDFLARE_GATEWAY_ID).cloned();
    let base_url = model
        .base_url
        .replace(&format!("{{{CLOUDFLARE_ACCOUNT_ID}}}"), account.as_deref().unwrap_or(&format!("{{{CLOUDFLARE_ACCOUNT_ID}}}")))
        .replace(&format!("{{{CLOUDFLARE_GATEWAY_ID}}}"), gateway.as_deref().unwrap_or(&format!("{{{CLOUDFLARE_GATEWAY_ID}}}")));
    if base_url == model.base_url { model.clone() } else { Model { base_url, ..model.clone() } }
}

struct CloudflareStreams {
    inner: Arc<dyn ProviderStreams>,
}

impl ProviderStreams for CloudflareStreams {
    fn stream(&self, model: &Model, context: &crate::types::Context, options: Option<StreamOptions>) -> crate::types::AssistantMessageEventStream {
        let resolved = resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.request.env.as_ref()));
        self.inner.stream(&resolved, context, options)
    }

    fn stream_simple(&self, model: &Model, context: &crate::types::Context, options: Option<SimpleStreamOptions>) -> crate::types::AssistantMessageEventStream {
        let resolved = resolve_cloudflare_model(model, options.as_ref().and_then(|o| o.stream.request.env.as_ref()));
        self.inner.stream_simple(&resolved, context, options)
    }
}

/// Wraps a provider's api implementation so Cloudflare account/gateway endpoint placeholders in
/// `baseUrl` materialize from the resolved provider env before dispatch.
pub fn cloudflare_streams(streams: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(CloudflareStreams { inner: streams })
}
