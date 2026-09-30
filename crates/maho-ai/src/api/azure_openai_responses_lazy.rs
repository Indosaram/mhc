//! Port of senpi packages/ai/src/api/azure-openai-responses.lazy.ts.

use std::sync::Arc;

use crate::api::azure_openai_responses;
use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::types::{AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};

struct AzureOpenAiResponsesModule;

impl ProviderStreams for AzureOpenAiResponsesModule {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        azure_openai_responses::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        azure_openai_responses::stream_simple(model, context, options)
    }
}

pub fn azure_openai_responses_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| Ok(Arc::new(AzureOpenAiResponsesModule) as Arc<dyn ProviderStreams>)),
        LazyApiCapabilities::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lazy_wrapper_forwards_to_the_concrete_module() {
        let api = azure_openai_responses_api();
        let mut model =
            crate::models_generated::get_builtin_provider_models("openai").expect("models")[0].clone();
        model.api = "azure-openai-responses".into();
        let stream = api.stream(&model, &Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert_eq!(message.error_message.as_deref(), Some("No API key for provider: openai"));
    }
}
