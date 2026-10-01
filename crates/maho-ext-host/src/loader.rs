use maho_ext_api::*;

pub struct NativeExtensionFactory {
    pub path: String,
    pub source_info: SourceInfo,
    pub extension: Box<dyn Extension>,
}
pub struct LoadExtensionsResult {
    pub extensions: Vec<LoadedExtension>,
    pub errors: Vec<ExtensionError>,
    pub runtime: ExtensionRuntime,
    pub events: EventBus,
}
pub fn load_extensions(factories: Vec<NativeExtensionFactory>, cwd: &std::path::Path, profile: ExtensionSessionProfile) -> LoadExtensionsResult {
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    for factory in factories {
        let runtime_checkpoint = runtime.registration_checkpoint();
        let events_checkpoint = events.registration_checkpoint();
        let mut api = ExtensionApi::new(LoadedExtension::new(&factory.path, cwd.to_owned(), factory.source_info), profile.clone(), events.clone(), runtime.clone());
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory.extension.register(&mut api))) {
            Ok(()) => extensions.push(api.registered),
            Err(payload) => {
                runtime.rollback_registration(runtime_checkpoint);
                events.rollback_registration(events_checkpoint);
                let message = payload.downcast_ref::<String>().cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_owned()))
                    .unwrap_or_else(|| "Native extension factory panicked".into());
                errors.push(ExtensionError { extension_path: factory.path, event: "load".into(), error: format!("Failed to load extension: {message}"), stack: None });
            }
        }
    }
    LoadExtensionsResult { extensions, errors, runtime, events }
}

pub type AsyncExtensionFactory = std::sync::Arc<dyn for<'a> Fn(&'a mut ExtensionApi) -> ExtensionFuture<'a, ()> + Send + Sync>;
pub struct NativeAsyncExtensionFactory {
    pub path: String,
    pub source_info: SourceInfo,
    pub factory: AsyncExtensionFactory,
}
pub async fn load_extensions_async(factories: Vec<NativeAsyncExtensionFactory>, cwd: &std::path::Path, profile: ExtensionSessionProfile) -> LoadExtensionsResult {
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    for factory in factories {
        let runtime_checkpoint = runtime.registration_checkpoint();
        let events_checkpoint = events.registration_checkpoint();
        let mut api = ExtensionApi::new(LoadedExtension::new(&factory.path, cwd.to_owned(), factory.source_info), profile.clone(), events.clone(), runtime.clone());
        let outcome = {
            let mut future = Box::pin(async { (factory.factory)(&mut api).await });
            std::future::poll_fn(|cx| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                    Ok(poll) => poll,
                    Err(payload) => {
                        let message = payload.downcast_ref::<String>().cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_owned()))
                            .unwrap_or_else(|| "Native extension factory panicked".into());
                        std::task::Poll::Ready(Err(ExtensionFailure::new(message)))
                    }
                }
            }).await
        };
        match outcome {
            Ok(()) => extensions.push(api.registered),
            Err(error) => {
                runtime.rollback_registration(runtime_checkpoint);
                events.rollback_registration(events_checkpoint);
                errors.push(ExtensionError { extension_path: factory.path, event: "load".into(), error: format!("Failed to load extension: {}", error.message), stack: error.stack });
            }
        }
    }
    LoadExtensionsResult { extensions, errors, runtime, events }
}
