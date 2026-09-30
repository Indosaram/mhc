//! Port of senpi packages/ai/src/api/mistral-conversations.lazy.ts.

use std::sync::Arc;

use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::api::mistral_conversations;
use crate::types::{AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};

struct MistralConversationsModule;

impl ProviderStreams for MistralConversationsModule {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        mistral_conversations::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        mistral_conversations::stream_simple(model, context, options)
    }
}

pub fn mistral_conversations_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| Ok(Arc::new(MistralConversationsModule) as Arc<dyn ProviderStreams>)),
        LazyApiCapabilities::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lazy_wrapper_forwards_to_the_concrete_module() {
        let api = mistral_conversations_api();
        let mut model =
            crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone();
        model.provider = "mistral".into();
        model.api = "mistral-conversations".into();
        let stream = api.stream(&model, &Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert_eq!(message.error_message.as_deref(), Some("No API key for provider: mistral"));
    }
}
