//! Port of senpi `packages/coding-agent/src/core/skill-discovery.ts`.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// `SKILL_FRONTMATTER_PREFIX_BYTES`.
pub const SKILL_FRONTMATTER_PREFIX_BYTES: usize = 8192;

/// `IGNORE_FILE_NAMES`.
pub const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

const SKIP_SKILL_WALK_DIRECTORY_NAMES: [&str; 2] = ["node_modules", ".git"];

const FRONTMATTER_CLOSE_LF: &[u8] = b"\n---";
const FRONTMATTER_CLOSE_CR: &[u8] = b"\r---";

/// `SkillDiscoveryMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiscoveryMode {
    Pi,
    Agents,
}

/// The npm `ignore` matcher: patterns added per directory, matched against posix-relative paths.
#[derive(Debug, Clone)]
pub struct IgnoreMatcher {
    builder: GitignoreBuilder,
    matcher: Gitignore,
}

impl Default for IgnoreMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl IgnoreMatcher {
    pub fn new() -> Self {
        let builder = GitignoreBuilder::new(".");
        let matcher = builder.build().unwrap_or_else(|_| Gitignore::empty());
        Self { builder, matcher }
    }

    /// `ig.add(patterns)`.
    pub fn add(&mut self, patterns: &[String]) {
        if patterns.is_empty() {
            return;
        }
        for pattern in patterns {
            let _ = self.builder.add_line(None, pattern);
        }
        if let Ok(matcher) = self.builder.build() {
            self.matcher = matcher;
        }
    }

    /// `ig.ignores(path)`; a trailing separator marks a directory.
    pub fn ignores(&self, path: &str) -> bool {
        let (path, is_dir) = match path.strip_suffix('/') {
            Some(stripped) => (stripped, true),
            None => (path, false),
        };
        self.matcher.matched_path_or_any_parents(path, is_dir).is_ignore()
    }
}

fn to_posix_path(path: &str) -> String {
    path.replace(std::path::MAIN_SEPARATOR, "/")
}

/// `shouldSkipSkillWalkDirectoryName`.
pub fn should_skip_skill_walk_directory_name(name: &str) -> bool {
    name.starts_with('.') || SKIP_SKILL_WALK_DIRECTORY_NAMES.contains(&name)
}

/// `prefixIgnorePattern`.
pub fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;

    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest;
    }

    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest;
    }

    let prefixed = format!("{prefix}{pattern}");
    Some(if negated { format!("!{prefixed}") } else { prefixed })
}

/// `addIgnoreRules`.
pub fn add_ignore_rules(ig: &mut IgnoreMatcher, dir: &str, root_dir: &str) {
    let relative_dir = relative_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() { String::new() } else { format!("{}/", to_posix_path(&relative_dir)) };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = Path::new(dir).join(filename);
        if !ignore_path.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&ignore_path) else {
            continue;
        };
        let patterns: Vec<String> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter_map(|line| prefix_ignore_pattern(line, &prefix))
            .collect();
        ig.add(&patterns);
    }
}

fn relative_path(root: &str, dir: &str) -> String {
    Path::new(dir).strip_prefix(Path::new(root)).map(|path| path.to_string_lossy().into_owned()).unwrap_or_default()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// `frontmatterPrefixEnd`: `None` when the closing fence is not inside the buffer.
fn frontmatter_prefix_end(buf: &[u8]) -> Option<usize> {
    let mut offset = 0usize;
    if buf.len() >= 3 && buf[0] == 0xef && buf[1] == 0xbb && buf[2] == 0xbf {
        offset = 3;
    }
    if buf.len() - offset < 3 || buf[offset] != 0x2d || buf[offset + 1] != 0x2d || buf[offset + 2] != 0x2d {
        return Some(buf.len());
    }
    let rest = &buf[offset + 3..];
    let lf = find_subslice(rest, FRONTMATTER_CLOSE_LF).map(|index| index + offset + 3);
    let cr = find_subslice(rest, FRONTMATTER_CLOSE_CR).map(|index| index + offset + 3);
    let close_at = match (lf, cr) {
        (Some(lf), Some(cr)) => Some(lf.min(cr)),
        (Some(lf), None) => Some(lf),
        (None, Some(cr)) => Some(cr),
        (None, None) => None,
    };
    close_at.map(|close_at| close_at + FRONTMATTER_CLOSE_LF.len())
}

fn decode_frontmatter_source(buf: &[u8]) -> String {
    let slice = match frontmatter_prefix_end(buf) {
        None => buf,
        Some(end) => &buf[..end.min(buf.len())],
    };
    String::from_utf8_lossy(slice).into_owned()
}

/// `readSkillMarkdownSource`: reads the frontmatter prefix, falling back to the whole file.
pub fn read_skill_markdown_source(file_path: &str) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(file_path)?;
    Ok(decode_frontmatter_source(&bytes))
}

/// `collectSkillEntries`.
pub fn collect_skill_entries(
    dir: &str,
    mode: SkillDiscoveryMode,
    ignore_matcher: Option<&IgnoreMatcher>,
    root_dir: Option<&str>,
) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    if !Path::new(dir).exists() {
        return entries;
    }

    let root = root_dir.unwrap_or(dir).to_string();
    let mut matcher = ignore_matcher.cloned().unwrap_or_default();
    add_ignore_rules(&mut matcher, dir, &root);

    let Ok(dir_entries) = std::fs::read_dir(dir) else {
        return entries;
    };
    let dir_entries: Vec<PathBuf> = dir_entries.flatten().map(|entry| entry.path()).collect();

    for full_path in &dir_entries {
        let Some(name) = full_path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            continue;
        };
        if name != "SKILL.md" {
            continue;
        }
        let is_file = match file_is(full_path) {
            Some(is_file) => is_file,
            None => continue,
        };
        let rel_path = to_posix_path(&relative_path(&root, &full_path.to_string_lossy()));
        if is_file && !matcher.ignores(&rel_path) {
            entries.push(full_path.to_string_lossy().into_owned());
            return entries;
        }
    }

    for full_path in &dir_entries {
        let Some(name) = full_path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            continue;
        };
        if should_skip_skill_walk_directory_name(&name) {
            continue;
        }

        let Some((is_dir, is_file)) = directory_and_file(full_path) else {
            continue;
        };

        let rel_path = to_posix_path(&relative_path(&root, &full_path.to_string_lossy()));
        let should_include_markdown_file = is_file
            && name.ends_with(".md")
            && !matcher.ignores(&rel_path)
            && ((mode == SkillDiscoveryMode::Pi && dir == root) || (mode == SkillDiscoveryMode::Agents && dir != root));
        if should_include_markdown_file {
            entries.push(full_path.to_string_lossy().into_owned());
            continue;
        }

        if !is_dir {
            continue;
        }
        if matcher.ignores(&format!("{rel_path}/")) {
            continue;
        }

        let nested = collect_skill_entries(&full_path.to_string_lossy(), mode, Some(&matcher), Some(&root));
        entries.extend(nested);
    }

    entries
}

fn file_is(path: &Path) -> Option<bool> {
    let file_type = std::fs::symlink_metadata(path).ok()?.file_type();
    if file_type.is_symlink() {
        Some(std::fs::metadata(path).ok()?.is_file())
    } else {
        Some(file_type.is_file())
    }
}

fn directory_and_file(path: &Path) -> Option<(bool, bool)> {
    let file_type = std::fs::symlink_metadata(path).ok()?.file_type();
    if file_type.is_symlink() {
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.is_dir(), metadata.is_file()))
    } else {
        Some((file_type.is_dir(), file_type.is_file()))
    }
}

/// `collectAutoSkillEntries`.
pub fn collect_auto_skill_entries(dir: &str, mode: SkillDiscoveryMode) -> Vec<String> {
    collect_skill_entries(dir, mode, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn dot_directories_and_node_modules_are_skipped() {
        assert!(should_skip_skill_walk_directory_name(".git"));
        assert!(should_skip_skill_walk_directory_name("node_modules"));
        assert!(should_skip_skill_walk_directory_name(".hidden"));
        assert!(!should_skip_skill_walk_directory_name("skills"));
    }

    #[test]
    fn ignore_patterns_are_prefixed_and_negations_kept() {
        assert_eq!(prefix_ignore_pattern("build/", "sub/"), Some("sub/build/".to_string()));
        assert_eq!(prefix_ignore_pattern("!keep.md", "sub/"), Some("!sub/keep.md".to_string()));
        assert_eq!(prefix_ignore_pattern("/root-only", ""), Some("root-only".to_string()));
        assert_eq!(prefix_ignore_pattern("# comment", ""), None);
        assert_eq!(prefix_ignore_pattern("   ", ""), None);
    }

    #[test]
    fn the_matcher_honours_added_patterns_and_negations() {
        let mut matcher = IgnoreMatcher::new();
        matcher.add(&["build/".to_string(), "!build/keep.md".to_string()]);
        assert!(matcher.ignores("build/"));
        assert!(matcher.ignores("build/x.txt"));
        assert!(!matcher.ignores("build/keep.md"));
        assert!(!matcher.ignores("src/main.rs"));
    }

    #[test]
    fn pi_mode_collects_root_markdown_and_agents_mode_only_nested() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("root.md"), "root");
        write_file(&root.join("nested").join("deep.md"), "deep");

        let pi_entries = collect_skill_entries(&root.to_string_lossy(), SkillDiscoveryMode::Pi, None, None);
        let pi_names: Vec<String> = pi_entries.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert_eq!(pi_names, vec!["root.md".to_string()]);

        let agents_entries = collect_skill_entries(&root.to_string_lossy(), SkillDiscoveryMode::Agents, None, None);
        let agents_names: Vec<String> = agents_entries.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert_eq!(agents_names, vec!["deep.md".to_string()]);
    }

    #[test]
    fn a_skill_directory_stops_the_walk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join("pack").join("SKILL.md"), "---\nname: pack\n---\nbody");
        write_file(&root.join("pack").join("extra").join("SKILL.md"), "---\nname: extra\n---\nbody");

        let entries = collect_skill_entries(&root.to_string_lossy(), SkillDiscoveryMode::Pi, None, None);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].ends_with("pack/SKILL.md"));
    }

    #[test]
    fn gitignore_files_inside_the_tree_filter_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(&root.join(".gitignore"), "ignored.md\n");
        write_file(&root.join("ignored.md"), "x");
        write_file(&root.join("kept.md"), "x");

        let entries = collect_skill_entries(&root.to_string_lossy(), SkillDiscoveryMode::Pi, None, None);
        let names: Vec<String> = entries.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert_eq!(names, vec!["kept.md".to_string()]);
    }

    #[test]
    fn reading_a_skill_source_keeps_the_frontmatter_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("SKILL.md");
        write_file(&path, "---\nname: demo\n---\nbody\n");
        assert_eq!(read_skill_markdown_source(&path.to_string_lossy()).expect("read"), "---\nname: demo\n---");
    }

    #[test]
    fn reading_a_source_without_frontmatter_returns_the_whole_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("SKILL.md");
        write_file(&path, "plain body");
        assert_eq!(read_skill_markdown_source(&path.to_string_lossy()).expect("read"), "plain body");
    }

    #[test]
    fn a_long_file_without_a_closing_fence_falls_back_to_the_whole_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("SKILL.md");
        let body = format!("---\nname: {}\n{}", "x".repeat(SKILL_FRONTMATTER_PREFIX_BYTES), "tail");
        write_file(&path, &body);
        assert_eq!(read_skill_markdown_source(&path.to_string_lossy()).expect("read"), body);
    }

    #[test]
    fn a_missing_skill_file_is_an_error() {
        assert!(read_skill_markdown_source("/definitely/missing/SKILL.md").is_err());
    }
}
