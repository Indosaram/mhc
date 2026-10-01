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

/// senpi RESOURCE_TYPES.
pub const RESOURCE_TYPES: [ResourceType; 5] = [
    ResourceType::Extensions,
    ResourceType::Skills,
    ResourceType::Prompts,
    ResourceType::Themes,
    ResourceType::Hooks,
];

/// senpi ResourceType.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Extensions,
    Skills,
    Prompts,
    Themes,
    Hooks,
}

impl ResourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
            Self::Hooks => "hooks",
        }
    }
}

/// senpi FILE_PATTERNS.
pub fn file_pattern(resource_type: ResourceType) -> &'static regex::Regex {
    use std::sync::LazyLock;
    static EXTENSIONS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.(ts|js)$").expect("extensions pattern"));
    static MARKDOWN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.md$").expect("markdown pattern"));
    static JSON: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.json$").expect("json pattern"));

    match resource_type {
        ResourceType::Extensions => &EXTENSIONS,
        ResourceType::Skills | ResourceType::Prompts => &MARKDOWN,
        ResourceType::Themes | ResourceType::Hooks => &JSON,
    }
}

/// senpi isPattern.
pub fn is_pattern(value: &str) -> bool {
    value.starts_with('!') || value.starts_with('+') || value.starts_with('-') || value.contains('*') || value.contains('?')
}

/// senpi isOverridePattern.
pub fn is_override_pattern(value: &str) -> bool {
    value.starts_with('!') || value.starts_with('+') || value.starts_with('-')
}

/// senpi hasGlobPattern.
pub fn has_glob_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

/// senpi splitPatterns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitPatterns {
    pub plain: Vec<String>,
    pub patterns: Vec<String>,
}

pub fn split_patterns(entries: &[String]) -> SplitPatterns {
    let mut split = SplitPatterns::default();
    for entry in entries {
        if is_pattern(entry) {
            split.patterns.push(entry.clone());
        } else {
            split.plain.push(entry.clone());
        }
    }
    split
}

/// senpi getExtensionTempFolder: the 0700 temp folder extension packages install into.
pub fn get_extension_temp_folder(agent_dir: &str) -> String {
    let temp_folder = std::path::Path::new(agent_dir).join("tmp").join("extensions").to_string_lossy().into_owned();
    let _ = std::fs::create_dir_all(&temp_folder);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_folder, std::fs::Permissions::from_mode(0o700));
    }
    temp_folder
}

/// senpi collectFiles: dot entries and node_modules are skipped, ignore files filter the walk.
pub fn collect_files(
    dir: &str,
    resource_type: ResourceType,
    skip_node_modules: bool,
    ignore_matcher: Option<&crate::skill_discovery::IgnoreMatcher>,
    root_dir: Option<&str>,
) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    if !std::path::Path::new(dir).exists() {
        return files;
    }

    let root = root_dir.unwrap_or(dir).to_string();
    let mut matcher = ignore_matcher.cloned().unwrap_or_default();
    crate::skill_discovery::add_ignore_rules(&mut matcher, dir, &root);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if skip_node_modules && name == "node_modules" {
            continue;
        }

        let full_path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let (is_dir, is_file) = if file_type.is_symlink() {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => (metadata.is_dir(), metadata.is_file()),
                Err(_) => continue,
            }
        } else {
            (file_type.is_dir(), file_type.is_file())
        };

        let rel_path = full_path
            .strip_prefix(std::path::Path::new(&root))
            .map(|path| path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
            .unwrap_or_default();
        let ignore_path = if is_dir { format!("{rel_path}/") } else { rel_path };
        if matcher.ignores(&ignore_path) {
            continue;
        }

        if is_dir {
            files.extend(collect_files(&full_path.to_string_lossy(), resource_type, skip_node_modules, Some(&matcher), Some(&root)));
        } else if is_file && file_pattern(resource_type).is_match(&name) {
            files.push(full_path.to_string_lossy().into_owned());
        }
    }

    files
}

/// senpi findGitRepoRoot.
pub fn find_git_repo_root(start_dir: &str) -> Option<String> {
    let mut dir = crate::paths::lexical_resolve(start_dir);
    loop {
        if std::path::Path::new(&dir).join(".git").exists() {
            return Some(dir);
        }
        let parent = std::path::Path::new(&dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != dir => dir = parent,
            _ => return None,
        }
    }
}

/// senpi collectAncestorAgentsSkillDirs.
pub fn collect_ancestor_agents_skill_dirs(start_dir: &str) -> Vec<String> {
    let mut skill_dirs: Vec<String> = Vec::new();
    let resolved_start_dir = crate::paths::lexical_resolve(start_dir);
    let git_repo_root = find_git_repo_root(&resolved_start_dir);

    let mut dir = resolved_start_dir;
    loop {
        skill_dirs.push(std::path::Path::new(&dir).join(".agents").join("skills").to_string_lossy().into_owned());
        if git_repo_root.as_deref() == Some(dir.as_str()) {
            break;
        }
        let parent = std::path::Path::new(&dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != dir => dir = parent,
            _ => break,
        }
    }

    skill_dirs
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use std::path::Path;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn patterns_are_detected_by_their_markers() {
        assert!(is_pattern("!keep"));
        assert!(is_pattern("skills/*.md"));
        assert!(is_pattern("?x"));
        assert!(!is_pattern("plain/path.md"));
        assert!(is_override_pattern("-drop"));
        assert!(!is_override_pattern("skills/*.md"));
        assert!(has_glob_pattern("a*b"));
        assert!(!has_glob_pattern("a/b"));
    }

    #[test]
    fn split_patterns_keeps_order_within_each_bucket() {
        let entries = vec!["a".to_string(), "!b".to_string(), "c".to_string(), "d*".to_string()];
        let split = split_patterns(&entries);
        assert_eq!(split.plain, vec!["a".to_string(), "c".to_string()]);
        assert_eq!(split.patterns, vec!["!b".to_string(), "d*".to_string()]);
    }

    #[test]
    fn file_patterns_follow_the_resource_type() {
        assert!(file_pattern(ResourceType::Extensions).is_match("x.ts"));
        assert!(file_pattern(ResourceType::Extensions).is_match("x.js"));
        assert!(!file_pattern(ResourceType::Extensions).is_match("x.json"));
        assert!(file_pattern(ResourceType::Skills).is_match("x.md"));
        assert!(file_pattern(ResourceType::Prompts).is_match("x.md"));
        assert!(file_pattern(ResourceType::Themes).is_match("x.json"));
        assert!(file_pattern(ResourceType::Hooks).is_match("x.json"));
        assert_eq!(RESOURCE_TYPES.len(), 5);
        assert_eq!(ResourceType::Skills.as_str(), "skills");
    }

    #[test]
    fn collection_skips_dot_entries_and_node_modules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("a.md"), "x");
        write(&root.join(".hidden.md"), "x");
        write(&root.join("node_modules").join("b.md"), "x");
        write(&root.join("nested").join("c.md"), "x");
        write(&root.join("nested").join("d.json"), "x");

        let files = collect_files(&root.to_string_lossy(), ResourceType::Skills, true, None, None);
        let names: Vec<String> = files.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert!(names.contains(&"a.md".to_string()));
        assert!(names.contains(&"c.md".to_string()));
        assert!(!names.contains(&".hidden.md".to_string()));
        assert!(!names.contains(&"b.md".to_string()));
        assert!(!names.contains(&"d.json".to_string()));
    }

    #[test]
    fn an_ignore_file_filters_the_collection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".gitignore"), "skip.md\n");
        write(&root.join("skip.md"), "x");
        write(&root.join("keep.md"), "x");
        let files = collect_files(&root.to_string_lossy(), ResourceType::Skills, true, None, None);
        let names: Vec<String> = files.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert_eq!(names, vec!["keep.md".to_string()]);
    }

    #[test]
    fn the_extension_temp_folder_is_created_with_owner_only_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let folder = get_extension_temp_folder(&dir.path().to_string_lossy());
        assert!(folder.ends_with("tmp/extensions"));
        assert!(Path::new(&folder).is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&folder).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn ancestor_agent_skill_dirs_stop_at_the_repository_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir");
        let dirs = collect_ancestor_agents_skill_dirs(&nested.to_string_lossy());
        assert_eq!(dirs.len(), 3);
        assert!(dirs[0].ends_with("a/b/.agents/skills"));
        assert!(dirs[2].ends_with("repo/.agents/skills"));
    }
}
