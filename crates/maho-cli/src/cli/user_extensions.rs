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
    MissingPath,
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

/// senpi `VENDORED_BUILTIN_EXTENSION_PACKAGES` (`resource-loader.ts`): third-party packages a builtin
/// already vendors. An installed copy is shadowed while its builtin is active.
pub const VENDORED_BUILTIN_EXTENSION_PACKAGES: [(&str, &str); 7] = [
    ("anthropic-bash", "pi-anthropic-bash"),
    ("anthropic-web-search", "pi-anthropic-web-search"),
    ("gpt-apply-patch", "pi-apply-patch"),
    ("bash-timeout", "pi-bash-timeout"),
    ("openai-web-search", "pi-openai-web-search"),
    ("todowrite", "pi-todotools"),
    ("codemode", "@code-yeongyu/senpi-codemode"),
];

/// senpi `shadowVendoredBuiltinExtensionPaths`: drop non-CLI extension paths whose nearest package
/// is a vendored copy of an active builtin.
pub fn shadow_vendored_builtin_extensions(resources: &mut Vec<ResolvedResource>, selection: &super::default_extensions::BuiltinExtensionSelection) {
    let shadowed: Vec<&str> = VENDORED_BUILTIN_EXTENSION_PACKAGES.iter()
        .filter(|(builtin, _)| selection.is_active(builtin)).map(|(_, package)| *package).collect();
    if shadowed.is_empty() { return; }
    resources.retain(|resource| {
        resource.metadata.source == "cli"
            || maho_core::package_identity::find_nearest_package_identity(&resource.path)
                .is_none_or(|identity| !shadowed.contains(&identity.package_name.as_str()))
    });
}

pub async fn resolve_and_discover(
    settings_manager: &mut SettingsManager,
    cwd: &str,
    agent_dir: &str,
    cli_extension_paths: &[String],
    no_extensions: bool,
) -> Result<UserExtensionDiscovery, PackageManagerError> {
    let selection = super::default_extensions::BuiltinExtensionSelection::from_settings(settings_manager);
    let manager = DefaultPackageManager::new(PackageManagerOptions { cwd, agent_dir, settings_manager });
    // senpi resource-loader.ts:701-703: `noExtensions` loads only the CLI extension paths.
    let mut paths = if no_extensions { ResolvedPaths::default() } else { manager.resolve(None).await? };
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
    shadow_vendored_builtin_extensions(&mut paths.extensions, &selection);
    let mut discovery = discover(&paths);
    discovery.errors.extend(missing_extension_path_errors(cwd, cli_extension_paths));
    Ok(discovery)
}

/// senpi resource-loader.ts:708-715: every local `-e` path that does not exist is reported as an
/// extension load error, `Extension path does not exist: <resolved>`.
pub fn missing_extension_path_errors(cwd: &str, cli_extension_paths: &[String]) -> Vec<UserExtensionError> {
    cli_extension_paths.iter()
        .filter(|path| maho_core::paths::is_local_path(path))
        .map(|path| maho_core::paths::resolve_path(path, cwd, &maho_core::paths::PathInputOptions { trim: true, ..Default::default() }))
        .filter(|resolved| !Path::new(resolved).exists())
        .map(|resolved| UserExtensionError {
            message: format!("Extension path does not exist: {resolved}"),
            path: resolved,
            kind: UserExtensionErrorKind::MissingPath,
        })
        .collect()
}

#[cfg(test)]
#[path = "user_extensions_tests.rs"]
mod tests;
