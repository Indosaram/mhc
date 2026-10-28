//! Rust port of `packages/telemetry-core/src/otel-traceparent-fetch.ts`.

use std::sync::Arc;

use crate::otel_providers::active_span_context;
use crate::otlp_http::OtlpFetch;

/// Our own hosts: the collector and the omo-service API. Model-provider hosts are never listed.
pub const OMO_TRACE_HOSTS: [&str; 2] = ["t.omo.dev", "api.omo.dev"];

pub fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_lowercase))
}

/// Adds the active span's `traceparent` to requests whose host is exactly one of `hosts`; every other
/// request, and every request made outside a span, passes through untouched.
pub fn create_traceparent_fetch(fetch: OtlpFetch, hosts: &[String]) -> OtlpFetch {
    let allowed: Vec<String> = hosts.iter().map(|host| host.to_lowercase()).collect();
    Arc::new(move |url, mut request| {
        let Some(host) = url_host(&url) else {
            return fetch(url, request);
        };
        if !allowed.contains(&host) {
            return fetch(url, request);
        }
        let Some(context) = active_span_context().filter(|context| context.is_valid()) else {
            return fetch(url, request);
        };
        let traceparent = context.traceparent();
        if !request
            .headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("traceparent"))
        {
            request
                .headers
                .push(("traceparent".to_string(), traceparent));
        }
        fetch(url, request)
    })
}
