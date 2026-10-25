//! Project path rules (upstream `project-paths.ts`): which recorded cwds can be offered as projects.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use super::session_history_fs::{FileStats, SessionHistoryFs, real_path_or, stat_or_null};

/// Lexical `path.resolve`: absolute against the process cwd, `.`/`..` collapsed, no trailing separator.
pub fn resolve_path(value: &str) -> String {
    let path = Path::new(value);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

fn join_path(base: &str, tail: &str) -> String {
    resolve_path(&Path::new(base).join(tail).to_string_lossy())
}

/// Expand a leading `~` the way a shell would; spawned CLIs record `~/...` verbatim in some setups.
pub fn expand_home_path(value: &str, home_dir: &str) -> String {
    if value == "~" {
        return home_dir.to_owned();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return join_path(home_dir, rest);
    }
    value.to_owned()
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/' || bytes[2] == b'\\')
}

fn is_drive_root(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn trim_trailing_separators(value: &str) -> String {
    if value.is_empty() || value == "/" || value == "\\" || is_drive_root(value) {
        return value.to_owned();
    }
    let trimmed = if value.starts_with('/') {
        value.trim_end_matches('/')
    } else {
        value.trim_end_matches(['\\', '/'])
    };
    if trimmed.is_empty() {
        return value.to_owned();
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return format!("{trimmed}\\");
    }
    trimmed.to_owned()
}

/// Comparison key for a project path: trailing separators dropped, Windows paths case folded.
pub fn normalize_project_path_for_comparison(value: &str) -> String {
    let normalized = trim_trailing_separators(value.trim());
    if is_windows_drive_path(&normalized) || normalized.starts_with("\\\\") {
        return normalized.replace('/', "\\").to_lowercase();
    }
    normalized
}

fn normalize_for_prefix_match(value: &str, case_fold: bool) -> String {
    let normalized = format!("{}/", value.replace('\\', "/"));
    if case_fold {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

#[derive(Clone)]
pub struct ProjectPathRulesInput {
    pub home_dir: Option<String>,
    pub tmp_dir: Option<String>,
    pub excluded_directories: Option<Vec<String>>,
    pub managed_worktree_directories: Option<Vec<String>>,
    /// Node-style platform name; `"win32"` case folds prefix matches.
    pub platform: Option<String>,
    pub fs: Arc<dyn SessionHistoryFs>,
}

pub fn default_home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".to_owned())
}

pub fn current_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

pub struct ProjectPathRules {
    pub home_dir: String,
    excluded_roots: HashSet<String>,
    excluded_ancestors: Vec<String>,
    case_fold: bool,
    fs: Arc<dyn SessionHistoryFs>,
}

/// Directories that are never offered as projects: the home and temp roots, Codex's per-conversation
/// scratch folders, Downloads, the app's own data directories, and OmO's disposable worktrees.
pub fn create_project_path_rules(input: ProjectPathRulesInput) -> ProjectPathRules {
    let home_dir = resolve_path(&input.home_dir.unwrap_or_else(default_home_dir));
    let case_fold = input.platform.as_deref().unwrap_or(current_platform()) == "win32";
    let tmp = input
        .tmp_dir
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    // `/private/tmp` is what macOS reports for sessions started in `/tmp`.
    let excluded_roots = [
        home_dir.clone(),
        tmp,
        "/tmp".to_owned(),
        "/private/tmp".to_owned(),
    ]
    .iter()
    .map(|directory| normalize_project_path_for_comparison(&resolve_path(directory)))
    .collect();
    let excluded = input
        .excluded_directories
        .unwrap_or_else(|| vec![join_path(&home_dir, ".omo")]);
    let mut ancestors = vec![
        join_path(&home_dir, "Downloads"),
        join_path(&join_path(&home_dir, "Documents"), "Codex"),
    ];
    ancestors.extend(excluded.iter().map(|d| resolve_path(d)));
    ancestors.extend(
        input
            .managed_worktree_directories
            .unwrap_or_default()
            .iter()
            .map(|d| resolve_path(d)),
    );
    let excluded_ancestors = ancestors
        .iter()
        .map(|d| normalize_for_prefix_match(d, case_fold))
        .collect();
    ProjectPathRules {
        home_dir,
        excluded_roots,
        excluded_ancestors,
        case_fold,
        fs: input.fs,
    }
}

impl ProjectPathRules {
    /// Absolute, resolved path for a recorded cwd, or `None` when it is relative or empty.
    pub fn resolve_recorded_path(&self, value: &str) -> Option<String> {
        let expanded = expand_home_path(value.trim(), &self.home_dir);
        (!expanded.is_empty() && Path::new(&expanded).is_absolute())
            .then(|| resolve_path(&expanded))
    }

    pub fn is_excluded_project_path(&self, candidate_path: &str) -> bool {
        if self
            .excluded_roots
            .contains(&normalize_project_path_for_comparison(candidate_path))
        {
            return true;
        }
        let normalized = normalize_for_prefix_match(candidate_path, self.case_fold);
        self.excluded_ancestors
            .iter()
            .any(|ancestor| normalized.starts_with(ancestor.as_str()))
            || normalized.contains("/.omo-app/worktrees/")
            || normalized.contains("/.t3/worktrees/")
    }

    /// Match directory aliases without assuming the volume is case-insensitive.
    pub fn directory_identity(&self, target: &str, known_stats: Option<&FileStats>) -> String {
        let resolved = resolve_path(target);
        let stats = known_stats
            .cloned()
            .or_else(|| stat_or_null(self.fs.as_ref(), &resolved));
        if let Some(stats) = &stats
            && let Some(ino) = stats.ino
        {
            return format!("inode:{}:{}", stats.dev, ino);
        }
        format!(
            "path:{}",
            normalize_project_path_for_comparison(&real_path_or(self.fs.as_ref(), &resolved))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::session_history_fs::node_session_history_fs;

    fn rules(platform: &str) -> ProjectPathRules {
        create_project_path_rules(ProjectPathRulesInput {
            home_dir: Some("/home/u".into()),
            tmp_dir: Some("/var/tmpx".into()),
            excluded_directories: None,
            managed_worktree_directories: Some(vec!["/wt".into()]),
            platform: Some(platform.into()),
            fs: node_session_history_fs(),
        })
    }

    #[test]
    fn expands_home_prefixes() {
        assert_eq!(expand_home_path("~", "/home/u"), "/home/u");
        // Expansion resolves the joined path, so on Windows the POSIX fixture gains the cwd's drive.
        assert_eq!(
            expand_home_path("~/code", "/home/u"),
            resolve_path("/home/u/code")
        );
        assert_eq!(expand_home_path("/abs", "/home/u"), "/abs");
    }

    #[test]
    fn normalizes_for_comparison() {
        assert_eq!(normalize_project_path_for_comparison("/a/b///"), "/a/b");
        assert_eq!(normalize_project_path_for_comparison("/"), "/");
        assert_eq!(
            normalize_project_path_for_comparison("C:/Users/Me/"),
            "c:\\users\\me"
        );
        assert_eq!(normalize_project_path_for_comparison("C:\\"), "c:\\");
        assert_eq!(
            normalize_project_path_for_comparison("\\\\Srv\\Share\\"),
            "\\\\srv\\share"
        );
    }

    /// Candidates reach the rules resolved (`resolve_recorded_path`); on Windows that gives the
    /// POSIX fixtures the cwd's drive, exactly as the rules' own roots get it.
    fn excluded(rules: &ProjectPathRules, path: &str) -> bool {
        rules.is_excluded_project_path(&resolve_path(path))
    }

    #[test]
    fn excludes_roots_scratch_and_worktrees() {
        let rules = rules("linux");
        for path in [
            "/home/u",
            "/var/tmpx",
            "/tmp",
            "/private/tmp",
            "/home/u/Downloads/x",
            "/home/u/Documents/Codex/a",
            "/home/u/.omo/agent",
            "/wt/branch",
            "/x/.omo-app/worktrees/y",
            "/x/.t3/worktrees/y",
        ] {
            assert!(excluded(&rules, path), "{path}");
        }
        assert!(!excluded(&rules, "/home/u/code/app"));
        assert!(!excluded(&rules, "/home/u/Downloadsx"));
    }

    #[test]
    fn win32_prefix_matching_is_case_folded() {
        assert!(excluded(&rules("win32"), "/HOME/U/downloads/x"));
        assert!(!excluded(&rules("linux"), "/HOME/U/downloads/x"));
    }

    #[test]
    fn recorded_paths_resolve_or_reject() {
        let rules = rules("linux");
        assert_eq!(
            rules.resolve_recorded_path(" ~/p/../q "),
            Some(resolve_path("/home/u/q"))
        );
        assert_eq!(rules.resolve_recorded_path("relative/dir"), None);
        assert_eq!(rules.resolve_recorded_path("  "), None);
    }
}
