//! Port of senpi packages/ai/src/api/google-generative-ai.lazy.ts.

use std::sync::Arc;

use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::api::google_generative_ai;
use crate::types::{AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};

struct GoogleGenerativeAiModule;

impl ProviderStreams for GoogleGenerativeAiModule {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        google_generative_ai::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        google_generative_ai::stream_simple(model, context, options)
    }
}

pub fn google_generative_ai_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| Ok(Arc::new(GoogleGenerativeAiModule) as Arc<dyn ProviderStreams>)),
        LazyApiCapabilities::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lazy_wrapper_forwards_to_the_concrete_module() {
        let api = google_generative_ai_api();
        let mut model =
            crate::models_generated::get_builtin_provider_models("google").expect("models")[0].clone();
        model.api = "google-generative-ai".into();
        model.provider = "google".into();
        let stream = api.stream(&model, &Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert!(message.error_message.as_deref().is_some_and(|message| message.starts_with("No API key for provider: ")));
    }
}
