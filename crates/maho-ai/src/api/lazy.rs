//! Port of senpi packages/ai/src/api/lazy.ts.

use std::future::Future;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

use crate::types::{
    AssistantMessageEventStream, BoxFuture, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    ErrorReason, Model, ProviderStreams, SimpleStreamOptions, StreamOptions,
};
use crate::utils::event_stream::create_assistant_message_event_stream;
use crate::utils::lazy::setup_error_message;

pub use crate::utils::lazy::setup_error_message as create_setup_error_message;

/// A stream that cancels its setup/forwarding task when the consumer drops it, mirroring
/// `LazyAssistantMessageEventStream`'s `return()` cancellation handler.
pub struct LazyStream {
    stream: AssistantMessageEventStream,
    task: tokio::task::JoinHandle<()>,
}

impl Deref for LazyStream {
    type Target = AssistantMessageEventStream;

    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl LazyStream {
    pub fn into_stream(self) -> AssistantMessageEventStream {
        let stream = self.stream.clone();
        std::mem::forget(self);
        stream
    }
}

impl Drop for LazyStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Returns a stream synchronously while running async setup (auth resolution, lazy module loading)
/// behind it. Setup failures terminate the stream with an error event.
pub fn lazy_stream(
    model: &Model,
    setup: impl Future<Output = Result<AssistantMessageEventStream, String>> + Send + 'static,
) -> LazyStream {
    let outer = create_assistant_message_event_stream();
    let forwarding = outer.clone();
    let error_model = model.clone();
    let task = tokio::spawn(async move {
        match setup.await {
            Ok(source) => {
                while let Ok(Some(event)) = source.next().await {
                    forwarding.push(event);
                }
                match source.result().await {
                    Ok(message) => forwarding.end(Some(message)),
                    Err(_) => forwarding.end(None),
                }
            }
            Err(message) => {
                let error = setup_error_message(&error_model, &message);
                forwarding.push(crate::types::AssistantMessageEvent::Error {
                    reason: ErrorReason::Error,
                    error: error.clone(),
                });
                forwarding.end(Some(error));
            }
        }
    });
    LazyStream { stream: outer, task }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LazyApiCapabilities {
    pub fetch_deferred: bool,
    pub cancel_deferred: bool,
}

type LoadModule = Arc<dyn Fn() -> Result<Arc<dyn ProviderStreams>, String> + Send + Sync>;

/// Wraps a module loader as `ProviderStreams`. The module loads on first stream call; the cached
/// result deduplicates loads. Load failures terminate the returned stream with an error event.
pub fn lazy_api(load: LoadModule, capabilities: LazyApiCapabilities) -> Arc<dyn ProviderStreams> {
    Arc::new(LazyApiProvider { load, capabilities, cached: OnceLock::new() })
}

struct LazyApiProvider {
    load: LoadModule,
    capabilities: LazyApiCapabilities,
    cached: OnceLock<Result<Arc<dyn ProviderStreams>, String>>,
}

impl LazyApiProvider {
    fn module(&self) -> Result<Arc<dyn ProviderStreams>, String> {
        self.cached.get_or_init(|| (self.load)()).clone()
    }
}

impl ProviderStreams for LazyApiProvider {
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let module = self.module();
        let model = model.clone();
        let context = context.clone();
        let call_model = model.clone();
        lazy_stream(&call_model, async move {
            let module = module?;
            Ok(module.stream(&model, &context, options))
        })
        .into_stream()
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let module = self.module();
        let model = model.clone();
        let context = context.clone();
        let call_model = model.clone();
        lazy_stream(&call_model, async move {
            let module = module?;
            Ok(module.stream_simple(&model, &context, options))
        })
        .into_stream()
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        if !self.capabilities.fetch_deferred {
            return None;
        }
        let module = self.module();
        let model = model.clone();
        let handle = handle.clone();
        let call_model = model.clone();
        Some(
            lazy_stream(&call_model, async move {
                let module = module?;
                module
                    .fetch_deferred(&model, &handle, options)
                    .ok_or_else(|| "API does not support deferred responses".to_owned())
            })
            .into_stream(),
        )
    }

    fn supports_deferred(&self) -> bool {
        self.capabilities.fetch_deferred
    }

    fn cancel_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> BoxFuture<'a, Result<(), String>> {
        if !self.capabilities.cancel_deferred {
            return Box::pin(async { Err("API cannot cancel deferred responses".to_owned()) });
        }
        let module = self.module();
        Box::pin(async move {
            let module = module?;
            module.cancel_deferred(model, handle, options).await
        })
    }

    fn supports_cancel_deferred(&self) -> bool {
        self.capabilities.cancel_deferred
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DoneReason, StopReason, Usage};
    use crate::utils::event_stream::StreamError;
    use std::time::Duration;

    fn model() -> Model {
        let mut model =
            crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone();
        model.api = "maho-lazy-test".into();
        model
    }

    fn message(stop_reason: StopReason) -> crate::types::AssistantMessage {
        let mut message = setup_error_message(&model(), "");
        message.stop_reason = stop_reason;
        message.error_message = None;
        message.usage = Usage::default();
        message
    }

    struct DoneProvider;

    impl ProviderStreams for DoneProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            let stream = create_assistant_message_event_stream();
            stream.push(crate::types::AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: message(StopReason::Stop),
            });
            stream
        }

        fn stream_simple(
            &self,
            model: &Model,
            context: &Context,
            options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            self.stream(model, context, options.map(|options| options.stream))
        }
    }

    #[tokio::test]
    async fn setup_failure_terminates_the_stream_with_the_ts_error_event() {
        let stream = lazy_stream(&model(), async { Err("boom".to_owned()) });
        let events = stream.collect().await.expect("events");
        let [crate::types::AssistantMessageEvent::Error { error, .. }] = events.as_slice() else {
            panic!("expected one error event, got {events:?}");
        };
        assert_eq!(error.error_message.as_deref(), Some("boom"));
        assert_eq!(stream.result().await.expect("result").error_message.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn setup_success_forwards_every_event_and_the_result() {
        let stream = lazy_stream(&model(), async {
            let inner = create_assistant_message_event_stream();
            inner.push(crate::types::AssistantMessageEvent::Start { partial: message(StopReason::Pending) });
            inner.push(crate::types::AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: message(StopReason::Stop),
            });
            Ok(inner)
        });
        let events = stream.collect().await.expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(stream.result().await.expect("result").stop_reason, StopReason::Stop);
    }

    #[tokio::test]
    async fn lazy_api_loads_once_and_rejects_missing_deferred_support() {
        let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = loads.clone();
        let provider = lazy_api(
            Arc::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Arc::new(DoneProvider) as Arc<dyn ProviderStreams>)
            }),
            LazyApiCapabilities { fetch_deferred: true, cancel_deferred: false },
        );
        let context = Context::default();
        let first = provider.stream(&model(), &context, None);
        assert_eq!(first.result().await.expect("result").stop_reason, StopReason::Stop);
        let second = provider.stream(&model(), &context, None);
        assert_eq!(second.result().await.expect("result").stop_reason, StopReason::Stop);
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(provider.supports_deferred());

        let handle = DeferredHandle {
            provider: "p".into(),
            model_id: "m".into(),
            api: "maho-lazy-test".into(),
            id: "d".into(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        };
        let deferred = provider.fetch_deferred(&model(), &handle, None).expect("deferred stream");
        let message = tokio::time::timeout(Duration::from_secs(5), deferred.result())
            .await
            .expect("bounded wait")
            .expect("result");
        assert_eq!(message.error_message.as_deref(), Some("API does not support deferred responses"));

        let unsupported = lazy_api(Arc::new(|| Err("nope".to_owned())), LazyApiCapabilities::default());
        assert!(unsupported.fetch_deferred(&model(), &handle, None).is_none());
        let failed = unsupported.stream(&model(), &context, None);
        assert_eq!(
            failed.result().await.map_err(|error: StreamError| error.message),
            Ok(message_with_error("nope"))
        );
    }

    fn message_with_error(text: &str) -> crate::types::AssistantMessage {
        let mut message = setup_error_message(&model(), text);
        message.stop_reason = StopReason::Error;
        message
    }

    /// Pinned `providers.test.ts` "lazily exposes only declared deferred capabilities": `lazyApi`
    /// attaches `cancelDeferred` only when the capability is declared, and forwards to the loaded
    /// module; an undeclared capability stays unsupported with the pinned error.
    #[tokio::test]
    async fn lazy_api_exposes_only_the_declared_cancel_capability_and_forwards_to_the_loaded_module() {
        struct CancelModule {
            cancelled: std::sync::Mutex<Vec<String>>,
        }
        impl ProviderStreams for CancelModule {
            fn stream(
                &self,
                _model: &Model,
                _context: &Context,
                _options: Option<StreamOptions>,
            ) -> AssistantMessageEventStream {
                create_assistant_message_event_stream()
            }
            fn stream_simple(
                &self,
                model: &Model,
                context: &Context,
                options: Option<SimpleStreamOptions>,
            ) -> AssistantMessageEventStream {
                self.stream(model, context, options.map(|options| options.stream))
            }
            fn cancel_deferred<'a>(
                &'a self,
                _model: &'a Model,
                handle: &'a DeferredHandle,
                _options: Option<crate::types::DeferredCancelOptions>,
            ) -> BoxFuture<'a, Result<(), String>> {
                let id = handle.id.clone();
                Box::pin(async move {
                    self.cancelled.lock().expect("cancelled").push(id);
                    Ok(())
                })
            }
            fn supports_cancel_deferred(&self) -> bool {
                true
            }
        }

        let module = Arc::new(CancelModule { cancelled: std::sync::Mutex::new(Vec::new()) });
        let loaded = module.clone();
        let provider = lazy_api(
            Arc::new(move || Ok(loaded.clone() as Arc<dyn ProviderStreams>)),
            LazyApiCapabilities { fetch_deferred: false, cancel_deferred: true },
        );
        assert!(provider.supports_cancel_deferred());
        let handle = DeferredHandle {
            provider: "p".into(),
            model_id: "m".into(),
            api: "maho-lazy-test".into(),
            id: "d".into(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        };
        provider.cancel_deferred(&model(), &handle, None).await.expect("cancel forwards to the module");
        assert_eq!(*module.cancelled.lock().expect("cancelled"), vec!["d".to_owned()]);

        let undeclared = lazy_api(Arc::new(|| Err("nope".to_owned())), LazyApiCapabilities::default());
        assert!(!undeclared.supports_cancel_deferred());
        assert_eq!(
            undeclared.cancel_deferred(&model(), &handle, None).await,
            Err("API cannot cancel deferred responses".to_owned())
        );
    }
}
