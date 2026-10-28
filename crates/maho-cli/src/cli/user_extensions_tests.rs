use maho_core::{package_manager::{PathMetadata, ResolvedPaths, ResolvedResource}, SourceOrigin, SourceScope};

use super::{discover, split_builtin_specifiers, UserExtensionErrorKind};

fn resource(path: &str, enabled: bool) -> ResolvedResource {
    ResolvedResource {
        path: path.to_owned(),
        enabled,
        metadata: PathMetadata { skill_provenance: None,
            source: "config".into(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/home/user/.omo/agent/extensions".into()),
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
    assert_eq!(result.factories[0].source_info.base_dir.as_deref(), Some("/home/user/.omo/agent/extensions"));
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

#[test]
fn missing_local_cli_extension_path_is_reported_and_remote_sources_are_not() {
    let dir = tempfile::tempdir().expect("cli extension directory");
    std::fs::write(dir.path().join("present.rs"), "").expect("present extension file");
    let cwd = dir.path().to_string_lossy().into_owned();
    let errors = super::missing_extension_path_errors(&cwd, &["./typo".into(), "present.rs".into(), "npm:pkg".into()]);

    let resolved = dir.path().join("typo").to_string_lossy().into_owned();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, UserExtensionErrorKind::MissingPath);
    assert_eq!(errors[0].path, resolved);
    assert_eq!(errors[0].message, format!("Extension path does not exist: {resolved}"));
}

#[test]
fn builtin_task_member_is_unknown() {
    let (sources, builtins) = split_builtin_specifiers(&["builtin:task-member".to_owned(), "/opt/ext/pi-ast-grep.rs".to_owned()]);

    assert_eq!(sources, vec!["/opt/ext/pi-ast-grep.rs".to_owned()]);
    assert!(builtins.factories.is_empty());
    assert_eq!(builtins.errors.len(), 1);
    assert_eq!(builtins.errors[0].kind, UserExtensionErrorKind::UnknownNativeExtension);
    assert_eq!(builtins.errors[0].path, "builtin:task-member");
}

#[test]
fn package_entries_map_to_their_port_beside_the_builtin_and_generated_shims_are_provided() {
    let root = tempfile::tempdir().expect("fixture root");
    let package = root.path().join("git/pi-ast-grep");
    std::fs::create_dir_all(package.join("src")).expect("package dir");
    std::fs::write(package.join("package.json"), r#"{"name":"pi-ast-grep"}"#).expect("manifest");
    std::fs::write(package.join("src/index.ts"), "").expect("entry");
    let rules = root.path().join("git/pi-rules");
    std::fs::create_dir_all(rules.join("src")).expect("rules dir");
    std::fs::write(rules.join("package.json"), r#"{"name":"@code-yeongyu/pi-rules"}"#).expect("rules manifest");
    std::fs::write(rules.join("src/index.ts"), "").expect("rules entry");
    let shim = root.path().join("diff.js");
    std::fs::write(&shim, maho_core::generated_shim_banner::generated_shim_banner()).expect("shim");
    let paths = ResolvedPaths { extensions: [package.join("src/index.ts"), rules.join("src/index.ts"), shim]
        .iter().map(|path| resource(&path.to_string_lossy(), true)).collect(), ..Default::default() };
    let result = discover(&paths);

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.factories.iter().map(|factory| factory.path.as_str()).collect::<Vec<_>>(), ["<user:pi-ast-grep>", "<user:pi-rules>"]);
}
