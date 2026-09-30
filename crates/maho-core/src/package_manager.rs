//! Port of senpi `packages/coding-agent/src/core/package-manager.ts` restricted to resource
//! packages (skills, prompts, themes); code extensions are native Rust crates (plan D-M5), so the
//! extension-package branches of the senpi file have no counterpart here.

use crate::source_info::SourceScope;

/// `PathMetadata`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathMetadata {
    pub source: String,
    pub scope: SourceScope,
    pub origin: crate::source_info::SourceOrigin,
    pub base_dir: Option<String>,
}

/// `ResolvedResource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResource {
    pub path: String,
    pub enabled: bool,
    pub metadata: PathMetadata,
}

/// `ResolvedPaths`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedPaths {
    pub extensions: Vec<ResolvedResource>,
    pub skills: Vec<ResolvedResource>,
    pub prompts: Vec<ResolvedResource>,
    pub themes: Vec<ResolvedResource>,
    pub hooks: Vec<ResolvedResource>,
}

/// `MissingSourceAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSourceAction {
    Install,
    Skip,
    Error,
}

/// `ProgressEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressEvent {
    pub event_type: ProgressEventType,
    pub action: ProgressAction,
    pub source: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEventType {
    Start,
    Progress,
    Complete,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressAction {
    Install,
    Remove,
    Update,
    Clone,
    Pull,
}

/// `PackageUpdate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageUpdate {
    pub source: String,
    pub display_name: String,
    pub package_type: PackageUpdateType,
    pub scope: InstalledSourceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageUpdateType {
    Npm,
    Git,
}

/// `InstalledSourceScope`: scopes whose packages the manager installs and updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledSourceScope {
    User,
    Project,
}

/// `ConfiguredPackage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredPackage {
    pub source: String,
    pub scope: InstalledSourceScope,
    pub filtered: bool,
    pub installed_path: Option<String>,
}

/// `resourcePrecedenceRank`: lower rank wins a name collision.
pub fn resource_precedence_rank(metadata: &PathMetadata) -> u8 {
    if metadata.origin == crate::source_info::SourceOrigin::Package {
        return 4;
    }
    let scope_base = if metadata.scope == SourceScope::Project { 0 } else { 2 };
    scope_base + u8::from(metadata.source != "local")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::SourceOrigin;

    fn metadata(source: &str, scope: SourceScope, origin: SourceOrigin) -> PathMetadata {
        PathMetadata { source: source.to_string(), scope, origin, base_dir: None }
    }

    #[test]
    fn package_resources_rank_last() {
        assert_eq!(resource_precedence_rank(&metadata("local", SourceScope::Project, SourceOrigin::Package)), 4);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::User, SourceOrigin::Package)), 4);
    }

    #[test]
    fn ranks_follow_project_then_user_then_discovered_order() {
        assert_eq!(resource_precedence_rank(&metadata("local", SourceScope::Project, SourceOrigin::TopLevel)), 0);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::Project, SourceOrigin::TopLevel)), 1);
        assert_eq!(resource_precedence_rank(&metadata("local", SourceScope::User, SourceOrigin::TopLevel)), 2);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::User, SourceOrigin::TopLevel)), 3);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::Temporary, SourceOrigin::TopLevel)), 3);
    }
}
