use maho_core::{package_manager::{PathMetadata, ResolvedPaths, ResolvedResource}, SourceOrigin, SourceScope};

use super::{discover, UserExtensionErrorKind};

fn resource(path: &str, enabled: bool) -> ResolvedResource {
    ResolvedResource {
        path: path.to_owned(),
        enabled,
        metadata: PathMetadata {
            source: "config".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/home/user/.maho/agent/extensions".into()),
        },
    }
}

#[test]
fn registered_native_identifier_produces_real_factory_with_source_provenance() {
    let paths = ResolvedPaths { extensions: vec![resource("/home/user/extensions/pi-ast-grep.rs", true)], ..Default::default() };
    let result = discover(&paths);

    assert!(result.errors.is_empty());
    assert_eq!(result.factories.len(), 1);
    assert_eq!(result.factories[0].path, "<user:pi-ast-grep>");
    assert_eq!(result.factories[0].source_info.path, paths.extensions[0].path);
    assert_eq!(result.factories[0].source_info.source, "config");
    assert_eq!(result.factories[0].source_info.scope, maho_ext_api::SourceScope::User);
    assert_eq!(result.factories[0].source_info.origin, maho_ext_api::SourceOrigin::TopLevel);
    assert_eq!(result.factories[0].source_info.base_dir.as_deref(), Some("/home/user/.maho/agent/extensions"));
}

#[test]
fn script_file_is_structurally_rejected_without_loading() {
    let paths = ResolvedPaths { extensions: vec![resource("/home/user/extensions/custom.ts", true)], ..Default::default() };
    let result = discover(&paths);

    assert!(result.factories.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].kind, UserExtensionErrorKind::UnsupportedScript);
    assert_eq!(result.errors[0].path, paths.extensions[0].path);
}

#[test]
fn unregistered_native_identifier_is_reported_without_placeholder_factory() {
    let paths = ResolvedPaths { extensions: vec![resource("/home/user/extensions/unknown.rs", true)], ..Default::default() };
    let result = discover(&paths);

    assert!(result.factories.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].kind, UserExtensionErrorKind::UnknownNativeExtension);
}

#[test]
fn disabled_resources_are_ignored_and_project_native_paths_are_supported() {
    let disabled = resource("/home/user/extensions/pi-ast-grep.rs", false);
    let mut project = resource("/work/extensions/pi-ast-grep.rs", true);
    project.metadata.scope = SourceScope::Project;
    let paths = ResolvedPaths { extensions: vec![disabled.clone(), project], ..Default::default() };
    let result = discover(&paths);

    assert_eq!(result.factories.len(), 1);
    assert!(result.errors.is_empty());
    assert_eq!(result.factories[0].source_info.scope, maho_ext_api::SourceScope::Project);
}

#[test]
fn cli_native_paths_keep_cli_source_and_temporary_trust_scope() {
    let mut cli = resource("/work/extension/pi-ast-grep.rs", true);
    cli.metadata.source = "cli".into();
    cli.metadata.scope = SourceScope::Temporary;
    let paths = ResolvedPaths { extensions: vec![cli], ..Default::default() };
    let result = discover(&paths);

    assert!(result.errors.is_empty());
    assert_eq!(result.factories.len(), 1);
    assert_eq!(result.factories[0].source_info.source, "cli");
    assert_eq!(result.factories[0].source_info.scope, maho_ext_api::SourceScope::Temporary);
}
