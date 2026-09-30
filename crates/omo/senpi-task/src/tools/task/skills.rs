//! `tools/task/skills.ts`: filesystem-backed `load_skills` resolution and the prompt prepend block.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::tools::task::types::{LoadedSkill, SkillLoader, SkillResolution};

/// Options passed to a skill directory discovery function.
pub struct SkillDiscoveryOptions<'a> {
    pub dir: &'a Path,
    pub source: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredSkill {
    pub name: String,
    pub file_path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillDiscovery {
    pub skills: Vec<DiscoveredSkill>,
}

/// Seam for the host's `loadSkillsFromDir`.
pub type DiscoverSkills =
    Arc<dyn Fn(&SkillDiscoveryOptions<'_>) -> SkillDiscovery + Send + Sync>;

#[derive(Clone, Default)]
pub struct FsSkillLoaderOptions {
    pub home_dir: Option<PathBuf>,
    pub agent_dir: Option<PathBuf>,
    pub extra_dirs: Vec<PathBuf>,
    pub load_skills_from_dir: Option<DiscoverSkills>,
}

/// v1 load_skills contract: wrap each resolved SKILL.md in a named block and place it before the
/// prompt. Empty input leaves the prompt untouched.
pub fn build_skill_prepend(skills: &[LoadedSkill], prompt: &str) -> String {
    if skills.is_empty() {
        return prompt.to_string();
    }
    let block = skills
        .iter()
        .map(|skill| match &skill.location {
            None => format!("<skill name=\"{}\">\n{}\n</skill>", skill.name, skill.content),
            Some(location) => [
                format!("<skill name=\"{}\" location=\"{}\">", skill.name, location),
                format!("References are relative to {}.", dirname(location)),
                String::new(),
                skill.content.clone(),
                "</skill>".to_string(),
            ]
            .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("{block}\n\n{prompt}")
}

fn dirname(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        Some(_) => ".".to_string(),
        None => path.to_string(),
    }
}

/// Lexical `path.resolve`: absolute against the process cwd, `.`/`..` collapsed, no symlink lookup.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
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
    out
}

fn ancestor_agent_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = resolve(cwd);
    loop {
        dirs.push(current.join(".agents").join("skills"));
        if current.join(".git").exists() {
            return dirs;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return dirs,
        }
    }
}

fn unique_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    dirs.into_iter()
        .map(|dir| resolve(&dir))
        .filter(|dir| seen.insert(dir.clone()))
        .collect()
}

fn search_dirs(cwd: &Path, home: &Path, agent_dir: &Path, extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.join(".senpi").join("skills")];
    dirs.extend(ancestor_agent_skill_dirs(cwd));
    dirs.push(cwd.join(".pi").join("skills"));
    dirs.push(agent_dir.join("skills"));
    dirs.push(home.join(".agents").join("skills"));
    dirs.extend(extra_dirs.iter().cloned());
    unique_dirs(dirs)
}

fn normalize_newlines(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

/// Returns (frontmatter, body-after-closing) when `normalized` opens with a frontmatter block.
fn split_frontmatter(normalized: &str) -> Option<(&str, &str)> {
    if !normalized.starts_with("---\n") {
        return None;
    }
    let closing = 4 + normalized[4..].find("\n---")?;
    Some((&normalized[4..closing], &normalized[closing + 4..]))
}

fn skill_body(raw: &str) -> String {
    let normalized = normalize_newlines(raw);
    match split_frontmatter(&normalized) {
        None => raw.to_string(),
        Some((_, rest)) => rest.strip_prefix('\n').unwrap_or(rest).trim().to_string(),
    }
}

fn frontmatter_name(raw: &str) -> Option<String> {
    let normalized = normalize_newlines(raw);
    let (header, _) = split_frontmatter(&normalized)?;
    header.lines().find_map(|line| {
        let value = line.strip_prefix("name:")?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'').trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn discovered(path: &Path, fallback: Option<&str>) -> Option<DiscoveredSkill> {
    let raw = fs::read_to_string(path).ok()?;
    let name = frontmatter_name(&raw).or_else(|| fallback.map(str::to_string))?;
    Some(DiscoveredSkill {
        name,
        file_path: path.to_path_buf(),
    })
}

fn collect_skills(dir: &Path, root: bool, out: &mut Vec<DiscoveredSkill>) {
    let skill_md = dir.join("SKILL.md");
    if skill_md.is_file() {
        let fallback = dir.file_name().map(|name| name.to_string_lossy().into_owned());
        out.extend(discovered(&skill_md, fallback.as_deref()));
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') || file_name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_skills(&path, false, out);
        } else if root && path.extension().is_some_and(|ext| ext == "md") {
            let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned());
            out.extend(discovered(&path, stem.as_deref()));
        }
    }
}

/// Default discovery: `SKILL.md` directories (recursive) plus root-level `*.md` skill files;
/// the frontmatter `name` wins over the directory/file name.
pub fn load_skills_from_dir(options: &SkillDiscoveryOptions<'_>) -> SkillDiscovery {
    let mut skills = Vec::new();
    collect_skills(options.dir, true, &mut skills);
    SkillDiscovery { skills }
}

fn read_skill_file(name: &str, path: &Path) -> Option<LoadedSkill> {
    let raw = fs::read_to_string(path).ok()?;
    Some(LoadedSkill {
        name: name.to_string(),
        content: skill_body(&raw),
        location: Some(path.to_string_lossy().into_owned()),
    })
}

fn direct_skill(name: &str, dir: &Path) -> Option<LoadedSkill> {
    let candidates = [dir.join(name).join("SKILL.md"), dir.join(format!("{name}.md"))];
    let path = candidates.iter().find(|candidate| candidate.exists())?;
    read_skill_file(name, path)
}

fn discovered_skill(
    name: &str,
    dir: &Path,
    discover: &DiscoverSkills,
    cache: &mut HashMap<PathBuf, SkillDiscovery>,
) -> Option<LoadedSkill> {
    if !dir.exists() {
        return None;
    }
    let discovery = cache.entry(dir.to_path_buf()).or_insert_with(|| {
        discover(&SkillDiscoveryOptions {
            dir,
            source: "project",
        })
    });
    let skill = discovery
        .skills
        .iter()
        .find(|candidate| candidate.name == name)?;
    read_skill_file(name, &skill.file_path)
}

fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn read_skill(
    name: &str,
    dirs: &[PathBuf],
    discover: &DiscoverSkills,
    cache: &mut HashMap<PathBuf, SkillDiscovery>,
) -> Option<LoadedSkill> {
    if !is_valid_skill_name(name) {
        return None;
    }
    dirs.iter().find_map(|dir| {
        direct_skill(name, dir).or_else(|| discovered_skill(name, dir, discover, cache))
    })
}

fn default_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Filesystem-backed loader. Searches project `.senpi/skills`, `~/.senpi/agent/skills`, then any extra
/// dirs (the omo-senpi plugin skills path is injected by the component). Missing names never fail.
pub fn create_fs_skill_loader(options: FsSkillLoaderOptions) -> Arc<SkillLoader> {
    let home = options.home_dir.unwrap_or_else(default_home_dir);
    let agent_dir = options
        .agent_dir
        .unwrap_or_else(|| home.join(".senpi").join("agent"));
    let extra_dirs = options.extra_dirs;
    let discover: DiscoverSkills = options
        .load_skills_from_dir
        .unwrap_or_else(|| Arc::new(load_skills_from_dir));
    Arc::new(move |names: &[String], cwd: &str| -> SkillResolution {
        let dirs = search_dirs(Path::new(cwd), &home, &agent_dir, &extra_dirs);
        let mut cache = HashMap::new();
        let mut skills: Vec<LoadedSkill> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for name in names {
            match read_skill(name, &dirs, &discover, &mut cache) {
                Some(skill) => skills.push(skill),
                None => missing.push(name.clone()),
            }
        }
        SkillResolution {
            prepend: build_skill_prepend(&skills, ""),
            resolved: skills.iter().map(|skill| skill.name.clone()).collect(),
            missing,
            skills: Some(skills),
        }
    })
}
