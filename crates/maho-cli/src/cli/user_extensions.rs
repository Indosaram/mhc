use std::{path::Path, sync::Arc};

use maho_core::{
    package_manager::{DefaultPackageManager, PackageManagerError, PackageManagerOptions, ResolvedPaths, ResolvedResource, ResolveSourcesOptions},
    settings_manager::SettingsManager,
    source_info,
};
use maho_ext_api::{Extension, SourceInfo};
use maho_ext_host::loader::NativeAsyncExtensionFactory;

use super::extension_registry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserExtensionErrorKind {
    UnsupportedScript,
    UnknownNativeExtension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserExtensionError {
    pub path: String,
    pub kind: UserExtensionErrorKind,
    pub message: String,
}

#[derive(Default)]
pub struct UserExtensionDiscovery {
    pub factories: Vec<NativeAsyncExtensionFactory>,
    pub errors: Vec<UserExtensionError>,
}

fn extension_id(path: &str) -> Option<&str> {
    let path = Path::new(path);
    path.file_stem()?.to_str()
}

fn report(resource: &ResolvedResource, kind: UserExtensionErrorKind, message: impl Into<String>, errors: &mut Vec<UserExtensionError>) {
    errors.push(UserExtensionError { path: resource.path.clone(), kind, message: message.into() });
}

fn source_info(resource: &ResolvedResource) -> SourceInfo {
    let info = source_info::create_source_info(&resource.path, &resource.metadata);
    SourceInfo {
        path: info.path, source: info.source, base_dir: info.base_dir,
        scope: match info.scope {
            source_info::SourceScope::User => maho_ext_api::SourceScope::User,
            source_info::SourceScope::Project => maho_ext_api::SourceScope::Project,
            source_info::SourceScope::Temporary => maho_ext_api::SourceScope::Temporary,
            source_info::SourceScope::System => maho_ext_api::SourceScope::System,
        },
        origin: match info.origin {
            source_info::SourceOrigin::Package => maho_ext_api::SourceOrigin::Package,
            source_info::SourceOrigin::TopLevel => maho_ext_api::SourceOrigin::TopLevel,
        },
    }
}

fn async_factory(resource: &ResolvedResource, extension: Box<dyn Extension>) -> NativeAsyncExtensionFactory {
    let path = resource.path.clone();
    let info = source_info(resource);
    let extension: Arc<dyn Extension> = Arc::from(extension);
    NativeAsyncExtensionFactory {
        path,
        source_info: info,
        factory: Arc::new(move |api| {
            extension.register(api);
            Box::pin(async { Ok(()) })
        }),
    }
}

pub fn discover(paths: &ResolvedPaths) -> UserExtensionDiscovery {
    let mut result = UserExtensionDiscovery::default();
    for resource in &paths.extensions {
        if !resource.enabled {
            continue;
        }
        let extension = Path::new(&resource.path);
        if let Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") = extension.extension().and_then(|extension| extension.to_str()) {
                report(resource, UserExtensionErrorKind::UnsupportedScript, "TypeScript/JavaScript extensions are unsupported; register this extension as a native crate", &mut result.errors);
                continue;
        }
        let Some(id) = extension_id(&resource.path) else {
            report(resource, UserExtensionErrorKind::UnknownNativeExtension, "Extension path has no valid native identifier", &mut result.errors);
            continue;
        };
        let Some(native) = extension_registry::user_extensions().iter().find(|native| native.id == id || format!("{}.rs", native.id) == Path::new(&resource.path).file_name().and_then(|name| name.to_str()).unwrap_or_default()) else {
            report(resource, UserExtensionErrorKind::UnknownNativeExtension, format!("No statically registered native extension named '{id}'"), &mut result.errors);
            continue;
        };
        let mut factory = async_factory(resource, (native.factory)());
        factory.path = format!("<user:{}>", native.id);
        result.factories.push(factory);
    }
    result
}

pub fn factories(paths: &ResolvedPaths) -> Result<Vec<NativeAsyncExtensionFactory>, Vec<UserExtensionError>> {
    let result = discover(paths);
    if result.errors.is_empty() { Ok(result.factories) } else { Err(result.errors) }
}

pub async fn resolve_and_discover(
    settings_manager: &mut SettingsManager,
    cwd: &str,
    agent_dir: &str,
    cli_extension_paths: &[String],
) -> Result<UserExtensionDiscovery, PackageManagerError> {
    let manager = DefaultPackageManager::new(PackageManagerOptions { cwd, agent_dir, settings_manager });
    let mut paths = manager.resolve(None).await?;
    if !cli_extension_paths.is_empty() {
        let cli_paths = manager.resolve_extension_sources(
            cli_extension_paths,
            Some(ResolveSourcesOptions { local: false, temporary: true }),
        ).await?;
        paths.extensions.extend(cli_paths.extensions.into_iter().map(|mut resource| {
            resource.metadata.source = "cli".into();
            resource
        }));
    }
    Ok(discover(&paths))
}

#[cfg(test)]
#[path = "user_extensions_tests.rs"]
mod tests;
