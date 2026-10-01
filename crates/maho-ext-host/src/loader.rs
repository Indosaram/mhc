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
    for factory in factories {
        let mut api = ExtensionApi::new(LoadedExtension::new(&factory.path, cwd.to_owned(), factory.source_info), profile.clone(), events.clone(), runtime.clone());
        factory.extension.register(&mut api);
        extensions.push(api.registered);
    }
    LoadExtensionsResult { extensions, errors: Vec::new(), runtime, events }
}
