//! Port of senpi packages/ai/src/bedrock-provider.ts.

use std::sync::Arc;

use crate::api::bedrock_converse_stream;
use crate::types::{
    AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions,
};

struct BedrockProviderModule;

impl ProviderStreams for BedrockProviderModule {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        bedrock_converse_stream::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        bedrock_converse_stream::stream_simple(model, context, options)
    }
}

pub fn bedrock_provider_module() -> Arc<dyn ProviderStreams> {
    Arc::new(BedrockProviderModule)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_module_forwards_to_the_converse_stream_implementation() {
        let module = bedrock_provider_module();
        let mut model =
            crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone();
        model.api = "bedrock-converse-stream".into();
        model.base_url = "http://127.0.0.1:1".into();
        let stream = module.stream(&model, &Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert_eq!(message.stop_reason, crate::types::StopReason::Error);
    }
}
