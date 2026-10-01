//! Port of senpi packages/ai/src/api/google-vertex.lazy.ts.

use std::sync::Arc;

use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::api::google_vertex;
use crate::types::{AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};

struct GoogleVertexModule;

impl ProviderStreams for GoogleVertexModule {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        google_vertex::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        google_vertex::stream_simple(model, context, options)
    }
}

pub fn google_vertex_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| Ok(Arc::new(GoogleVertexModule) as Arc<dyn ProviderStreams>)),
        LazyApiCapabilities::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lazy_wrapper_forwards_to_the_concrete_module() {
        let api = google_vertex_api();
        let mut model =
            crate::models_generated::get_builtin_provider_models("google").expect("models")[0].clone();
        model.api = "google-vertex".into();
        model.provider = "google-vertex".into();
        let stream = api.stream(&model, &Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert!(
            message
                .error_message
                .as_deref()
                .is_some_and(|message| message.starts_with("Vertex AI requires a project ID.")),
            "{:?}",
            message.error_message
        );
    }
}
