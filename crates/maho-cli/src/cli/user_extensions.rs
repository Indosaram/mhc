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

enum NativeResolution { Port(&'static extension_registry::NativeExtension), Provided, Unresolved }

/// Map one resolved extension path to the linked port that replaces its TypeScript entry: the file
/// (or `<id>/index.*` directory) name, then the nearest package name. Generated global-default shims
/// are already provided natively. senpi shadows only `VENDORED_BUILTIN_EXTENSION_PACKAGES`
/// (`resource-loader.ts:114-122`), so standalone packages a builtin also vendors still load.
fn native_resolution(path: &str) -> NativeResolution {
    let file = Path::new(path);
    if std::fs::read_to_string(file).is_ok_and(|content| maho_core::generated_shim_banner::has_generated_shim_banner(&content)) {
        return NativeResolution::Provided;
    }
    let stem = file.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
    let named = if stem == "index" { file.parent().and_then(Path::file_name).and_then(|name| name.to_str()).unwrap_or_default() } else { stem };
    let package = maho_core::package_identity::find_nearest_package_identity(path).map(|identity| identity.package_name);
    let package = package.as_deref().map(|name| name.rsplit('/').next().unwrap_or(name));
    if let Some(native) = extension_registry::user_extensions().iter().find(|native| native.id == named || Some(native.id) == package) {
        return NativeResolution::Port(native);
    }
    NativeResolution::Unresolved
}

pub fn discover(paths: &ResolvedPaths) -> UserExtensionDiscovery {
    let mut result = UserExtensionDiscovery::default();
    for resource in &paths.extensions {
        if !resource.enabled {
            continue;
        }
        match native_resolution(&resource.path) {
            NativeResolution::Provided => continue,
            NativeResolution::Port(native) => {
                if result.factories.iter().any(|factory| factory.path == format!("<user:{}>", native.id)) { continue; }
                let mut factory = async_factory(resource, (native.factory)());
                factory.path = format!("<user:{}>", native.id);
                result.factories.push(factory);
                continue;
            }
            NativeResolution::Unresolved => {}
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
        report(resource, UserExtensionErrorKind::UnknownNativeExtension, format!("No statically registered native extension named '{id}'"), &mut result.errors);
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

/// `builtin:<id>` names a natively linked entry a parent passes to a child process through
/// `--extension` (the native stand-in for an on-disk bundle path such as `omo-member.js`).
pub const BUILTIN_EXTENSION_SPECIFIER_PREFIX: &str = "builtin:";

fn builtin_cli_factory(specifier: &str, id: &str) -> Result<NativeAsyncExtensionFactory, UserExtensionError> {
    // No extension is natively linked through `builtin:<id>` (the omo `task-member` was removed).
    let extension: Option<Arc<dyn Extension>> = None;
    let Some(extension) = extension else {
        return Err(UserExtensionError { path: specifier.to_owned(), kind: UserExtensionErrorKind::UnknownNativeExtension, message: format!("No natively linked extension named '{id}'") });
    };
    let path = format!("<builtin:{id}>");
    Ok(NativeAsyncExtensionFactory {
        source_info: SourceInfo { path: path.clone(), source: "cli".into(), scope: maho_ext_api::SourceScope::Temporary, ..SourceInfo::default() },
        path,
        factory: Arc::new(move |api| { extension.register(api); Box::pin(async { Ok(()) }) }),
    })
}

/// Splits `builtin:<id>` specifiers off the CLI `--extension` sources; they resolve to linked
/// factories instead of package-manager paths.
fn split_builtin_specifiers(cli_extension_paths: &[String]) -> (Vec<String>, UserExtensionDiscovery) {
    let mut sources = Vec::new();
    let mut builtins = UserExtensionDiscovery::default();
    for source in cli_extension_paths {
        match source.trim().strip_prefix(BUILTIN_EXTENSION_SPECIFIER_PREFIX) {
            Some(id) => match builtin_cli_factory(source, id) {
                Ok(factory) => builtins.factories.push(factory),
                Err(error) => builtins.errors.push(error),
            },
            None => sources.push(source.clone()),
        }
    }
    (sources, builtins)
}

async fn resolve_cli_extension_paths(manager: &DefaultPackageManager<'_>, sources: &[String]) -> Result<Vec<ResolvedResource>, PackageManagerError> {
    if sources.is_empty() { return Ok(Vec::new()); }
    let cli_paths = manager.resolve_extension_sources(sources, Some(ResolveSourcesOptions { local: false, temporary: true })).await?;
    Ok(cli_paths.extensions.into_iter().map(|mut resource| {
        resource.metadata.source = "cli".into();
        resource
    }).collect())
}

fn merge_discovery(mut discovery: UserExtensionDiscovery, builtins: UserExtensionDiscovery) -> UserExtensionDiscovery {
    discovery.factories.extend(builtins.factories);
    discovery.errors.extend(builtins.errors);
    discovery
}

pub async fn resolve_and_discover(
    settings_manager: &mut SettingsManager,
    cwd: &str,
    agent_dir: &str,
    cli_extension_paths: &[String],
    no_extensions: bool,
) -> Result<UserExtensionDiscovery, PackageManagerError> {
    let selection = super::default_extensions::BuiltinExtensionSelection::from_settings(settings_manager);
    let (sources, builtins) = split_builtin_specifiers(cli_extension_paths);
    let manager = DefaultPackageManager::new(PackageManagerOptions { cwd, agent_dir, settings_manager });
    // senpi resource-loader.ts:701-703: `noExtensions` loads only the CLI extension paths.
    let mut paths = if no_extensions { ResolvedPaths::default() } else { manager.resolve(None).await? };
    paths.extensions.extend(resolve_cli_extension_paths(&manager, &sources).await?);
    shadow_vendored_builtin_extensions(&mut paths.extensions, &selection);
    let mut discovery = discover(&paths);
    // `builtin:<id>` specifiers are linked entries, not filesystem paths.
    discovery.errors.extend(missing_extension_path_errors(cwd, &sources));
    Ok(merge_discovery(discovery, builtins))
}

/// senpi resource-loader.ts:708-715: every local `-e` path that does not exist is reported as an
/// extension load error, `Extension path does not exist: <resolved>`.
pub fn missing_extension_path_errors(cwd: &str, cli_extension_paths: &[String]) -> Vec<UserExtensionError> {
    let home = maho_core::config::home_dir();
    cli_extension_paths.iter()
        .filter(|path| maho_core::paths::is_local_path(path))
        .map(|path| maho_core::paths::resolve_path(path, cwd, &maho_core::paths::PathInputOptions { trim: true, home_dir: Some(home.clone()), ..Default::default() }))
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
