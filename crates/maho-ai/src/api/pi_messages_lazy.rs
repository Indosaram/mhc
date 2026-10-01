//! Port of senpi packages/ai/src/api/pi-messages.lazy.ts.

use std::sync::Arc;

use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::api::pi_messages;
use crate::types::{AssistantMessageEventStream, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};

struct PiMessagesModule;

impl ProviderStreams for PiMessagesModule {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        pi_messages::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        pi_messages::stream_simple(model, context, options)
    }
}

pub fn pi_messages_api() -> Arc<dyn ProviderStreams> {
    lazy_api(Arc::new(|| Ok(Arc::new(PiMessagesModule) as Arc<dyn ProviderStreams>)), LazyApiCapabilities::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lazy_wrapper_forwards_to_the_concrete_module() {
        let api = pi_messages_api();
        let mut model =
            crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone();
        model.provider = "radius".into();
        model.api = "pi-messages".into();
        let stream = api.stream(&model, &Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert_eq!(
            message.error_message.as_deref(),
            Some(format!("No API key provided for provider \"{}\"", model.provider).as_str())
        );
    }
}
