//! Repository analysis and the choice of directories that get a context file (spec §1–3).

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Directory names never walked.
pub const SKIPPED_DIRS: [&str; 9] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
];

const LANGUAGES: [(&str, &str); 28] = [
    ("rs", "Rust"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("mjs", "JavaScript"),
    ("py", "Python"),
    ("go", "Go"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("swift", "Swift"),
    ("c", "C"),
    ("h", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("hpp", "C++"),
    ("cs", "C#"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("scala", "Scala"),
    ("lua", "Lua"),
    ("sh", "Shell"),
    ("zig", "Zig"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("dart", "Dart"),
    ("vue", "Vue"),
    ("svelte", "Svelte"),
];

const MANIFESTS: [&str; 16] = [
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "CMakeLists.txt",
    "Makefile",
    "Gemfile",
    "composer.json",
    "deno.json",
    "mix.exs",
    "pubspec.yaml",
    "*.csproj",
];

const ENTRY_STEMS: [&str; 6] = ["main", "index", "app", "cli", "lib", "mod"];
const TEST_DIRS: [&str; 4] = ["test", "tests", "__tests__", "spec"];

/// Context file names, preferred first.
pub const CONTEXT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// The language of a file name, by extension.
pub fn language_of(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    LANGUAGES
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, l)| *l)
}

fn is_manifest(name: &str) -> bool {
    MANIFESTS.iter().any(|m| match m.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => *m == name,
    })
}

fn is_entry(name: &str) -> bool {
    if name == "__init__.py" || name == "lib.rs" || name == "mod.rs" {
        return true;
    }
    let Some((stem, _)) = name.split_once('.') else {
        return false;
    };
    ENTRY_STEMS.contains(&stem) && language_of(name).is_some()
}

fn is_test_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.rsplit_once('.').map_or(lower.as_str(), |(s, _)| s);
    stem.ends_with("_test")
        || stem.contains(".test")
        || stem.contains(".spec")
        || (lower.starts_with("test_") && lower.ends_with(".py"))
}

/// Facts about one directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DirFacts {
    /// Relative path from the root, `.` for the root, `/` separated.
    pub path: String,
    pub depth: usize,
    pub files: usize,
    pub code_files: usize,
    pub subtree_code_files: usize,
    /// Language → files in the subtree.
    pub languages: BTreeMap<String, usize>,
    pub manifests: Vec<String>,
    pub entry_points: Vec<String>,
    pub has_tests: bool,
    pub context_file: Option<String>,
    /// Relative paths of the walked child directories.
    pub child_dirs: Vec<String>,
}

impl DirFacts {
    /// The top languages (at most 3), most files first.
    pub fn top_languages(&self) -> Vec<String> {
        let mut list: Vec<(&String, &usize)> = self.languages.iter().collect();
        list.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        list.into_iter().take(3).map(|(l, _)| l.clone()).collect()
    }
}

/// The analysis of one root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub root: PathBuf,
    /// Every walked directory by relative path.
    pub dirs: BTreeMap<String, DirFacts>,
}

fn rel(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let text = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if text.is_empty() { ".".into() } else { text }
}

fn parent_of(path: &str) -> Option<String> {
    if path == "." {
        return None;
    }
    Some(
        path.rsplit_once('/')
            .map_or(".".into(), |(p, _)| p.to_owned()),
    )
}

/// The git top-level containing `cwd`, else `cwd`.
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    cwd.to_path_buf()
}

/// Walks `root` honouring ignore files and the skip list.
pub fn analyze(root: &Path) -> Analysis {
    let mut dirs: BTreeMap<String, DirFacts> = BTreeMap::new();
    dirs.insert(
        ".".into(),
        DirFacts {
            path: ".".into(),
            ..DirFacts::default()
        },
    );
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .ignore(true)
        .parents(false)
        .require_git(false)
        .filter_entry(|entry| {
            !(entry.file_type().is_some_and(|t| t.is_dir())
                && SKIPPED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()))
        })
        .build();
    for entry in walker.flatten() {
        let Some(kind) = entry.file_type() else {
            continue;
        };
        let path = rel(root, entry.path());
        if kind.is_dir() {
            if path == "." {
                continue;
            }
            let depth = path.split('/').count();
            dirs.entry(path.clone()).or_insert_with(|| DirFacts {
                path: path.clone(),
                depth,
                ..DirFacts::default()
            });
            if let Some(parent) = parent_of(&path) {
                let name = path.rsplit('/').next().unwrap_or_default().to_owned();
                let facts = dirs.entry(parent.clone()).or_default();
                facts.child_dirs.push(path.clone());
                if TEST_DIRS.contains(&name.as_str()) {
                    facts.has_tests = true;
                }
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let parent = parent_of(&path).unwrap_or_else(|| ".".into());
        let language = language_of(&name);
        let facts = dirs.entry(parent.clone()).or_default();
        facts.files += 1;
        if language.is_some() {
            facts.code_files += 1;
        }
        if is_manifest(&name) {
            facts.manifests.push(name.clone());
        }
        if is_entry(&name) {
            facts.entry_points.push(name.clone());
        }
        if is_test_file(&name) {
            facts.has_tests = true;
        }
        if CONTEXT_FILES.contains(&name.as_str())
            && facts.context_file.as_deref() != Some(CONTEXT_FILES[0])
        {
            facts.context_file = Some(name.clone());
        }
        if let Some(language) = language {
            let mut current = Some(parent);
            while let Some(dir) = current {
                let facts = dirs.entry(dir.clone()).or_default();
                facts.subtree_code_files += 1;
                *facts.languages.entry(language.to_owned()).or_default() += 1;
                current = parent_of(&dir);
            }
        }
    }
    for (path, facts) in &mut dirs {
        facts.path.clone_from(path);
        facts.depth = if path == "." {
            0
        } else {
            path.split('/').count()
        };
        facts.manifests.sort();
        facts.entry_points.sort();
        facts.child_dirs.sort();
    }
    Analysis {
        root: root.to_path_buf(),
        dirs,
    }
}
