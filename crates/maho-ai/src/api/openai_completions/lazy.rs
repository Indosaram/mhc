//! Port of senpi packages/ai/src/api/lazy.ts (the `lazyStream`/`lazyApi` surface).
//!
//! Node-private copy: `src/api/lazy.rs` is owned by node 12-misc. Rust has no dynamic import, so
//! `load` is an infallible-or-failing closure over an already-compiled module; the observable
//! contract (a stream returned synchronously while setup runs behind it, setup failures terminating
//! the stream with an error event) is preserved.

use crate::model::Model;
use crate::types::{
    AssistantMessageEvent, AssistantMessageEventStream, Context, ProviderStreams, SimpleStreamOptions, StreamOptions,
};
use crate::utils::lazy::setup_error_message;
use std::sync::Arc;

pub type LazyLoad = Arc<dyn Fn() -> Result<Arc<dyn ProviderStreams>, String> + Send + Sync>;

pub fn lazy_stream<F>(model: Model, setup: F) -> AssistantMessageEventStream
where
    F: std::future::Future<Output = Result<AssistantMessageEventStream, String>> + Send + 'static,
{
    let outer = AssistantMessageEventStream::assistant();
    let target = outer.clone();
    tokio::spawn(async move {
        match setup.await {
            Ok(source) => {
                loop {
                    match source.next().await {
                        Ok(Some(event)) => target.push(event),
                        Ok(None) => break,
                        Err(error) => {
                            target.fail(error);
                            return;
                        }
                    }
                }
                target.end(source.result().await.ok());
            }
            Err(message) => {
                let error = setup_error_message(&model, &message);
                target.push(AssistantMessageEvent::Error { reason: crate::types::ErrorReason::Error, error: error.clone() });
                target.end(Some(error));
            }
        }
    });
    outer
}

struct LazyApi {
    load: LazyLoad,
}

impl ProviderStreams for LazyApi {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        let load = self.load.clone();
        let context = context.clone();
        let model = model.clone();
        let setup_model = model.clone();
        lazy_stream(model, async move {
            let api = load()?;
            Ok(api.stream(&setup_model, &context, options))
        })
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let load = self.load.clone();
        let context = context.clone();
        let model = model.clone();
        let setup_model = model.clone();
        lazy_stream(model, async move {
            let api = load()?;
            Ok(api.stream_simple(&setup_model, &context, options))
        })
    }
}

pub fn lazy_api(load: LazyLoad) -> Arc<dyn ProviderStreams> {
    Arc::new(LazyApi { load })
}
