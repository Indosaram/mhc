//! Port of senpi `packages/coding-agent/src/core/source-info.ts`.

use crate::package_manager::PathMetadata;

/// `SourceScope`: where a resource comes from. `System` marks resources the harness itself provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
    User,
    Project,
    #[default]
    Temporary,
    System,
}

impl SourceScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Temporary => "temporary",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "project" => Some(Self::Project),
            "temporary" => Some(Self::Temporary),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// `SourceOrigin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceOrigin {
    Package,
    #[default]
    TopLevel,
}

impl SourceOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Package => "package",
            Self::TopLevel => "top-level",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "package" => Some(Self::Package),
            "top-level" => Some(Self::TopLevel),
            _ => None,
        }
    }
}

/// `SourceInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    pub base_dir: Option<String>,
}

/// `createSourceInfo`.
pub fn create_source_info(path: &str, metadata: &PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: metadata.source.clone(),
        scope: metadata.scope,
        origin: metadata.origin,
        base_dir: metadata.base_dir.clone(),
    }
}

/// Options of `createSyntheticSourceInfo`.
#[derive(Debug, Clone, Default)]
pub struct SyntheticSourceInfoOptions {
    pub source: String,
    pub scope: Option<SourceScope>,
    pub origin: Option<SourceOrigin>,
    pub base_dir: Option<String>,
}

/// `createSyntheticSourceInfo`.
pub fn create_synthetic_source_info(path: &str, options: SyntheticSourceInfoOptions) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: options.source,
        scope: options.scope.unwrap_or(SourceScope::Temporary),
        origin: options.origin.unwrap_or(SourceOrigin::TopLevel),
        base_dir: options.base_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_source_info_defaults_to_temporary_top_level() {
        let info = create_synthetic_source_info("/tmp/skill.md", SyntheticSourceInfoOptions { source: "local".to_string(), ..Default::default() });
        assert_eq!(info.source, "local");
        assert_eq!(info.scope, SourceScope::Temporary);
        assert_eq!(info.origin, SourceOrigin::TopLevel);
        assert_eq!(info.base_dir, None);
    }

    #[test]
    fn synthetic_source_info_keeps_explicit_scope_and_base_dir() {
        let info = create_synthetic_source_info(
            "/tmp/skill.md",
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::User),
                origin: None,
                base_dir: Some("/tmp".to_string()),
            },
        );
        assert_eq!(info.scope, SourceScope::User);
        assert_eq!(info.base_dir.as_deref(), Some("/tmp"));
    }

    #[test]
    fn source_metadata_copies_every_field() {
        let metadata = PathMetadata {
            source: "auto".to_string(),
            scope: SourceScope::Project,
            origin: SourceOrigin::Package,
            base_dir: Some("/pkg".to_string()),
        };
        let info = create_source_info("/pkg/skills/a/SKILL.md", &metadata);
        assert_eq!(info.path, "/pkg/skills/a/SKILL.md");
        assert_eq!(info.source, "auto");
        assert_eq!(info.scope, SourceScope::Project);
        assert_eq!(info.origin, SourceOrigin::Package);
        assert_eq!(info.base_dir.as_deref(), Some("/pkg"));
    }

    #[test]
    fn scope_and_origin_round_trip_through_their_wire_spellings() {
        for scope in [SourceScope::User, SourceScope::Project, SourceScope::Temporary, SourceScope::System] {
            assert_eq!(SourceScope::parse(scope.as_str()), Some(scope));
        }
        for origin in [SourceOrigin::Package, SourceOrigin::TopLevel] {
            assert_eq!(SourceOrigin::parse(origin.as_str()), Some(origin));
        }
    }
}
