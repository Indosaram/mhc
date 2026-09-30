//! Port of senpi packages/ai/src/providers/opencode-headers.ts.

use crate::types::{
    AssistantMessageEventStream, Context, Model, ProviderHeaders, ProviderStreams, SimpleStreamOptions, StreamOptions,
};
use std::sync::Arc;

const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let expected = name.to_lowercase();
    headers.is_some_and(|headers| headers.keys().any(|key| key.to_lowercase() == expected))
}

fn with_session_header(options: StreamOptions) -> StreamOptions {
    let mut options = options;
    let Some(session_id) = options.session_id.clone() else { return options };
    if has_header(options.request.headers.as_ref(), OPENCODE_SESSION_HEADER) {
        return options;
    }
    let mut headers = options.request.headers.clone().unwrap_or_default();
    headers.insert(OPENCODE_SESSION_HEADER.to_owned(), Some(session_id));
    options.request.headers = Some(headers);
    options
}

struct OpenCodeSessionHeaders {
    inner: Arc<dyn ProviderStreams>,
}

impl ProviderStreams for OpenCodeSessionHeaders {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options.map(with_session_header))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.inner
            .stream_simple(model, context, options.map(|options| SimpleStreamOptions { stream: with_session_header(options.stream), ..options }))
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &crate::types::DeferredHandle,
        options: Option<crate::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn supports_deferred(&self) -> bool {
        self.inner.supports_deferred()
    }
}

pub fn with_open_code_session_header(streams: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(OpenCodeSessionHeaders { inner: streams })
}
