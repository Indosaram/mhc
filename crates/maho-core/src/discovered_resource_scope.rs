//! Port of senpi `packages/coding-agent/src/core/discovered-resource-scope.ts`.

use std::path::Path;

use crate::package_manager::PathMetadata;
use crate::source_info::{SourceInfo, SourceScope};

/// `DiscoveredResourceEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredResourceEntry {
    pub path: String,
    pub extension_path: String,
    pub scope: Option<SourceScope>,
}

/// `DiscoveredResourcePath`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredResourcePath {
    pub path: String,
    pub metadata: PathMetadata,
}

/// `ContributingExtension`: senpi's `Pick<Extension, "path" | "sourceInfo">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributingExtension {
    pub path: String,
    pub source_info: SourceInfo,
}

/// `getExtensionSourceLabel`.
pub fn get_extension_source_label(extension_path: &str) -> String {
    if extension_path.starts_with('<') {
        return format!("extension:{}", extension_path.replace(['<', '>'], ""));
    }
    let base = Path::new(extension_path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let stripped = base.strip_suffix(".ts").or_else(|| base.strip_suffix(".js")).map(str::to_string).unwrap_or(base);
    format!("extension:{stripped}")
}

/// `resolveDiscoveredResourcePaths`.
pub fn resolve_discovered_resource_paths(
    entries: &[DiscoveredResourceEntry],
    extensions: &[ContributingExtension],
) -> Vec<DiscoveredResourcePath> {
    entries
        .iter()
        .map(|entry| {
            let contributor = extensions.iter().find(|extension| extension.path == entry.extension_path);
            let scope = entry.scope.unwrap_or_else(|| match contributor {
                Some(contributor) if inherits_system_scope(contributor, &entry.path) => SourceScope::System,
                _ => SourceScope::Temporary,
            });
            DiscoveredResourcePath {
                path: entry.path.clone(),
                metadata: PathMetadata {
                    source: get_extension_source_label(&entry.extension_path),
                    scope,
                    origin: crate::source_info::SourceOrigin::TopLevel,
                    base_dir: if entry.extension_path.starts_with('<') {
                        None
                    } else {
                        Path::new(&entry.extension_path).parent().map(|parent| parent.to_string_lossy().into_owned())
                    },
                },
            }
        })
        .collect()
}

fn inherits_system_scope(contributor: &ContributingExtension, resource_path: &str) -> bool {
    if contributor.source_info.scope != SourceScope::System {
        return false;
    }
    if contributor.path.starts_with("<builtin:") {
        return true;
    }
    let Some(package_root) = contributor.source_info.base_dir.as_deref() else {
        return false;
    };
    is_under_path(&crate::paths::lexical_resolve(resource_path), &crate::paths::lexical_resolve(package_root))
}

fn is_under_path(target: &str, root: &str) -> bool {
    if target == root {
        return true;
    }
    let prefix = if root.ends_with(std::path::MAIN_SEPARATOR) { root.to_string() } else { format!("{root}{}", std::path::MAIN_SEPARATOR) };
    target.starts_with(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::SyntheticSourceInfoOptions;

    fn extension(path: &str, scope: SourceScope, base_dir: Option<&str>) -> ContributingExtension {
        ContributingExtension {
            path: path.to_string(),
            source_info: crate::source_info::create_synthetic_source_info(
                path,
                SyntheticSourceInfoOptions { source: "local".to_string(), scope: Some(scope), origin: None, base_dir: base_dir.map(str::to_string) },
            ),
        }
    }

    fn entry(path: &str, extension_path: &str, scope: Option<SourceScope>) -> DiscoveredResourceEntry {
        DiscoveredResourceEntry { path: path.to_string(), extension_path: extension_path.to_string(), scope }
    }

    #[test]
    fn labels_name_the_file_or_the_angle_bracket_placeholder() {
        assert_eq!(get_extension_source_label("/ext/demo.ts"), "extension:demo");
        assert_eq!(get_extension_source_label("/ext/demo.js"), "extension:demo");
        assert_eq!(get_extension_source_label("<builtin:codemode>"), "extension:builtin:codemode");
        assert_eq!(get_extension_source_label("/ext/other.mjs"), "extension:other.mjs");
    }

    #[test]
    fn an_explicit_scope_wins() {
        let extensions = vec![extension("/ext/a.ts", SourceScope::System, Some("/ext"))];
        let resolved = resolve_discovered_resource_paths(&[entry("/ext/skills/s.md", "/ext/a.ts", Some(SourceScope::Project))], &extensions);
        assert_eq!(resolved[0].metadata.scope, SourceScope::Project);
    }

    #[test]
    fn builtin_system_extensions_contribute_system_scope() {
        let extensions = vec![extension("<builtin:codemode>", SourceScope::System, None)];
        let resolved = resolve_discovered_resource_paths(&[entry("/anywhere/skill.md", "<builtin:codemode>", None)], &extensions);
        assert_eq!(resolved[0].metadata.scope, SourceScope::System);
        assert_eq!(resolved[0].metadata.source, "extension:builtin:codemode");
        assert_eq!(resolved[0].metadata.base_dir, None);
        assert_eq!(resolved[0].metadata.origin, crate::source_info::SourceOrigin::TopLevel);
    }

    #[test]
    fn a_system_package_only_inherits_for_paths_inside_its_payload() {
        let extensions = vec![extension("/pkg/ext.ts", SourceScope::System, Some("/pkg"))];
        let resolved = resolve_discovered_resource_paths(
            &[entry("/pkg/skills/s.md", "/pkg/ext.ts", None), entry("/elsewhere/s.md", "/pkg/ext.ts", None)],
            &extensions,
        );
        assert_eq!(resolved[0].metadata.scope, SourceScope::System);
        assert_eq!(resolved[1].metadata.scope, SourceScope::Temporary);
    }

    #[test]
    fn a_non_system_contributor_keeps_the_temporary_scope() {
        let extensions = vec![extension("/pkg/ext.ts", SourceScope::Project, Some("/pkg"))];
        let resolved = resolve_discovered_resource_paths(&[entry("/pkg/skills/s.md", "/pkg/ext.ts", None)], &extensions);
        assert_eq!(resolved[0].metadata.scope, SourceScope::Temporary);
        assert_eq!(resolved[0].metadata.base_dir.as_deref(), Some("/pkg"));
    }

    #[test]
    fn an_unknown_contributor_is_temporary_and_keeps_the_extension_label() {
        let resolved = resolve_discovered_resource_paths(&[entry("/pkg/s.md", "/pkg/unknown.ts", None)], &[]);
        assert_eq!(resolved[0].metadata.scope, SourceScope::Temporary);
        assert_eq!(resolved[0].metadata.source, "extension:unknown");
        assert_eq!(resolved[0].metadata.base_dir.as_deref(), Some("/pkg"));
    }
}
