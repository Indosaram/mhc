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
struct FactoryScopeGuard { runtime: ExtensionRuntime, events: EventBus, committed: bool }
impl Drop for FactoryScopeGuard {
    fn drop(&mut self) {
        if !self.committed {
            self.runtime.invalidate_registration("Extension factory failed to load");
            self.events.invalidate_registration();
        }
    }
}
pub fn load_extensions(factories: Vec<NativeExtensionFactory>, cwd: &std::path::Path, profile: ExtensionSessionProfile) -> LoadExtensionsResult {
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    for factory in factories {
        let mut api = ExtensionApi::new(LoadedExtension::new(&factory.path, cwd.to_owned(), factory.source_info), profile.clone(), events.registration_scope(), runtime.registration_scope());
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory.extension.register(&mut api))) {
            Ok(()) => match api.runtime.commit_registration() {
                Ok(()) => extensions.push(api.registered),
                Err(error) => {
                    api.runtime.invalidate_registration("Extension factory failed to load");
                    api.events.invalidate_registration();
                    errors.push(ExtensionError { extension_path: factory.path, event: "load".into(), error: format!("Failed to load extension: {}", error.message), stack: error.stack });
                }
            },
            Err(payload) => {
                api.runtime.invalidate_registration("Extension factory failed to load");
                api.events.invalidate_registration();
                let failure = payload.downcast_ref::<ExtensionFailure>();
                let message = failure.map(|error| error.message.clone()).or_else(|| payload.downcast_ref::<String>().cloned())
                    .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_owned()))
                    .unwrap_or_else(|| "Native extension factory panicked".into());
                errors.push(ExtensionError { extension_path: factory.path, event: "load".into(), error: format!("Failed to load extension: {message}"), stack: failure.and_then(|error| error.stack.clone()) });
            }
        }
    }
    errors.extend(detect_extension_conflicts(&extensions));
    LoadExtensionsResult { extensions, errors, runtime, events }
}

pub type AsyncExtensionFactory = std::sync::Arc<dyn for<'a> Fn(&'a mut ExtensionApi) -> ExtensionFuture<'a, ()> + Send + Sync>;
#[derive(Clone)]
pub struct NativeAsyncExtensionFactory {
    pub path: String,
    pub source_info: SourceInfo,
    pub factory: AsyncExtensionFactory,
}
pub struct NativeInlineExtension {
    pub name: Option<String>,
    pub hidden: bool,
    pub factory: AsyncExtensionFactory,
}
pub struct LoadInlineExtensionsResult {
    pub loaded: LoadExtensionsResult,
    pub hidden_paths: std::collections::BTreeSet<String>,
}
pub async fn load_inline_extensions(factories: Vec<NativeInlineExtension>, cwd: &std::path::Path, profile: ExtensionSessionProfile) -> LoadInlineExtensionsResult {
    let mut hidden_paths = std::collections::BTreeSet::new();
    let factories = factories.into_iter().enumerate().map(|(index, extension)| {
        let path = format!("<inline:{}>", extension.name.unwrap_or_else(|| index.saturating_add(1).to_string()));
        if extension.hidden { hidden_paths.insert(path.clone()); }
        NativeAsyncExtensionFactory { source_info: SourceInfo { path: path.clone(), source: "inline".into(), ..Default::default() }, path, factory: extension.factory }
    }).collect();
    let mut loaded = load_extensions_async(factories, cwd, profile).await;
    hidden_paths.retain(|path| loaded.extensions.iter().any(|extension| extension.identity.path == *path));
    for extension in &mut loaded.extensions { extension.hidden = hidden_paths.contains(&extension.identity.path); }
    LoadInlineExtensionsResult { loaded, hidden_paths }
}
pub async fn load_extensions_async(factories: Vec<NativeAsyncExtensionFactory>, cwd: &std::path::Path, profile: ExtensionSessionProfile) -> LoadExtensionsResult {
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    for factory in factories {
        let mut api = ExtensionApi::new(LoadedExtension::new(&factory.path, cwd.to_owned(), factory.source_info), profile.clone(), events.registration_scope(), runtime.registration_scope());
        let mut scope = FactoryScopeGuard { runtime: api.runtime.clone(), events: api.events.clone(), committed: false };
        let outcome = {
            let mut future = Box::pin(async { (factory.factory)(&mut api).await });
            std::future::poll_fn(|cx| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                    Ok(poll) => poll,
                    Err(payload) => {
                        if let Some(error) = payload.downcast_ref::<ExtensionFailure>() { return std::task::Poll::Ready(Err(error.clone())); }
                        let message = payload.downcast_ref::<String>().cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_owned()))
                            .unwrap_or_else(|| "Native extension factory panicked".into());
                        std::task::Poll::Ready(Err(ExtensionFailure::new(message)))
                    }
                }
            }).await
        };
        let outcome = outcome.and_then(|()| api.runtime.commit_registration());
        match outcome {
            Ok(()) => { scope.committed = true; extensions.push(api.registered); }
            Err(error) => {
                api.runtime.invalidate_registration("Extension factory failed to load");
                api.events.invalidate_registration();
                errors.push(ExtensionError { extension_path: factory.path, event: "load".into(), error: format!("Failed to load extension: {}", error.message), stack: error.stack });
            }
        }
    }
    errors.extend(detect_extension_conflicts(&extensions));
    LoadExtensionsResult { extensions, errors, runtime, events }
}

/// senpi `shouldSuppressExtensionConflict`: a conflict involving a builtin is expected (builtins are
/// overridable). The upstream same-package-identity case never applies natively: native extension
/// paths are synthetic `<kind:id>` identities, which have no package identity.
fn should_suppress_extension_conflict(existing_owner: &str, candidate_owner: &str) -> bool {
    existing_owner.starts_with("<builtin:") || candidate_owner.starts_with("<builtin:")
}

/// senpi `detectExtensionConflicts`: tools and flags registered under the same name by different
/// extensions are reported (all extensions stay loaded; load order decides precedence).
pub fn detect_extension_conflicts(extensions: &[LoadedExtension]) -> Vec<ExtensionError> {
    let mut conflicts = Vec::new();
    let mut tool_owners: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut flag_owners: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut report = |path: &str, error: String| conflicts.push(ExtensionError { extension_path: path.into(), event: "load".into(), error, stack: None });
    for extension in extensions {
        let path = extension.identity.path.as_str();
        for tool in &extension.tools {
            let name = tool.definition.name.as_str();
            match tool_owners.get(name) {
                Some(existing) if *existing != path => {
                    if !should_suppress_extension_conflict(existing, path) { report(path, format!("Tool \"{name}\" conflicts with {existing}")); }
                }
                Some(_) => {}
                None => { tool_owners.insert(name, path); }
            }
        }
        for flag in &extension.flags {
            let name = flag.name.as_str();
            match flag_owners.get(name) {
                Some(existing) if *existing != path => {
                    if !should_suppress_extension_conflict(existing, path) { report(path, format!("Flag \"--{name}\" conflicts with {existing}")); }
                }
                Some(_) => {}
                None => { flag_owners.insert(name, path); }
            }
        }
    }
    conflicts
}
