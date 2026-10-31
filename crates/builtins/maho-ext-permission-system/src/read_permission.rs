//! Port of `permission-system/read-permission.ts` and the `getBundledResourceRoots` it consults
//! (`bundled-resources.ts`).
use crate::{
    external_dir::{
        is_external_path, resolve_external_path, resolve_without_open,
        to_parent_directory_pattern_from,
    },
    parsers::{PermissionRequest, resolve_read_path},
};
use std::path::{Path, PathBuf};

/// Shipped payload roots, not user-discovered skill or extension directories (senpi
/// `getBundledResourceRoots`): themes, export template, interactive assets, docs and examples
/// under the package directory, the directory of the running binary, and the materialized imagegen
/// skill.
///
/// DIVERGENCE: mhc links its builtin extensions into the binary, so there are no bundled npm
/// packages (`bundledBuiltinExtensions`) to add; the imagegen skill lives where
/// `maho_ext_imagegen::embedded_skill_path` materializes it (computed here without writing it).
pub fn bundled_resource_roots() -> Vec<PathBuf> {
    let package = PathBuf::from(maho_core::config::get_package_dir());
    let mut roots = vec![
        package.join("theme"),
        package.join("export-html"),
        package.join("assets"),
        PathBuf::from(maho_core::config::get_docs_path()),
        PathBuf::from(maho_core::config::get_examples_path()),
        std::env::temp_dir()
            .join(format!("maho-imagegen-{}", env!("CARGO_PKG_VERSION")))
            .join("skill"),
        package,
    ];
    if let Some(binary_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        roots.push(binary_dir);
    }
    roots
}

fn uniq(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// Resolve the same file the read tool will open before classifying its ownership. A file inside
/// a bundled resource root is auto-approved when the rules only ask (a deny still decides); any
/// other file is a plain read plus, when it lies outside `cwd`, an `external_directory` request with
/// one canonical target, its lexical spelling as a rule alias and the raw path for display.
pub fn parse_read_permission(
    file_path: &str,
    location: (&Path, &Path),
    bundled_roots: &[PathBuf],
) -> Vec<PermissionRequest> {
    let (cwd, home) = location;
    let read_path = resolve_read_path(file_path, cwd);
    let target = resolve_without_open(Path::new(&read_path))
        .to_string_lossy()
        .into_owned();
    let request = PermissionRequest {
        permission: "read".into(),
        patterns: vec![file_path.to_owned()],
        always: vec![file_path.to_owned()],
        ..PermissionRequest::default()
    };
    if bundled_roots
        .iter()
        .any(|root| !is_external_path(&target, root, home))
    {
        // The service still evaluates denial rules; only an ask result is auto-approved.
        return vec![PermissionRequest {
            patterns: vec![read_path.clone()],
            always: vec![read_path.clone()],
            auto_approve_ask: true,
            rule_aliases: Some(uniq([file_path.to_owned(), read_path, target])),
            ..request
        }];
    }
    let Some(external) = resolve_external_path(&target, cwd, home) else {
        return vec![request];
    };
    vec![
        request,
        PermissionRequest {
            permission: "external_directory".into(),
            patterns: vec![external.clone()],
            always: vec![to_parent_directory_pattern_from(
                &external, false, file_path,
            )],
            rule_aliases: (read_path != external).then(|| vec![read_path.clone()]),
            original_paths: (file_path != external).then(|| vec![file_path.to_owned()]),
            auto_approve_ask: false,
        },
    ]
}
