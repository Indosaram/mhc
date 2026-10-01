//! Port of senpi packages/ai/src/api/bedrock-converse-stream.lazy.ts.

use std::sync::{Arc, Mutex, OnceLock};

use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::types::ProviderStreams;

static BEDROCK_MODULE_OVERRIDE: OnceLock<Mutex<Option<Arc<dyn ProviderStreams>>>> = OnceLock::new();

fn override_slot() -> &'static Mutex<Option<Arc<dyn ProviderStreams>>> {
    BEDROCK_MODULE_OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Overrides the bedrock implementation used by the lazy wrapper. Used by hosts whose bundled
/// binary cannot load the concrete module lazily; such a host registers a statically linked module.
pub fn set_bedrock_provider_module(module: Arc<dyn ProviderStreams>) {
    *override_slot().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(module);
}

pub fn bedrock_converse_stream_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| {
            let overridden = override_slot().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            Ok(overridden.unwrap_or_else(crate::bedrock_provider::bedrock_provider_module))
        }),
        LazyApiCapabilities::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn override_replaces_the_concrete_module() {
        set_bedrock_provider_module(crate::bedrock_provider::bedrock_provider_module());
        let api = bedrock_converse_stream_api();
        let mut model =
            crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone();
        model.api = "bedrock-converse-stream".into();
        model.base_url = "http://127.0.0.1:1".into();
        let stream = api.stream(&model, &crate::types::Context::default(), None);
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert_eq!(message.stop_reason, crate::types::StopReason::Error);
    }
}
