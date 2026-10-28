//! Port of senpi `interactive-mode.ts` `showLoadedResources` and its path/label helpers
//! (`formatDisplayPath`, `getShortPath`, `getCompact*Label`, `formatDiagnostics`).
//!
//! `InteractiveMode` gathers the loaded resources from the session; this module turns them into the
//! `[Context]`/`[Skills]`/`[Prompts]`/`[Extensions]`/`[Themes]` sections and the diagnostic notices
//! mounted in its loaded-resources container.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;

use maho_core::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use maho_core::source_info::SourceInfo;
use maho_tui::components::spacer::Spacer;
use maho_tui::tui::Component;

use crate::components::earendil_announcement::{NoticeLine, NoticeSpec, build_notice_box};
use crate::components::loaded_resource_section::LoadedResourceSection;
use crate::loaded_resource_scopes::{
    ResourceScopeGroupFormat, ScopedResource, build_resource_scope_groups, format_resource_scope_groups,
    get_display_source_info, is_package_source_info, is_system_resource,
};
use crate::theme::{Theme, ThemeColor};

static NPM_ROOT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(.*/node_modules)/(@?[^/]+(?:/[^/]+)?)$").expect("static npm root pattern"));
static NPM_PATH: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"node_modules/(@?[^/]+(?:/[^/]+)?)/(.*)").expect("static npm path pattern"));
static GIT_PATH: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"git/[^/]+/[^/]+/(.*)").expect("static git path pattern"));
static BUILTIN_PATH: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^<builtin:([^>]+)>$").expect("static builtin path pattern"));

/// A prompt template as listed by `showLoadedResources` (`session.promptTemplates`).
pub struct LoadedPrompt {
    pub name: String,
    pub file_path: String,
    pub source_info: Option<SourceInfo>,
}

/// A skill as listed by `showLoadedResources` (`resourceLoader.getSkills().skills`).
pub struct LoadedSkill {
    pub name: String,
    pub file_path: String,
    pub source_info: Option<SourceInfo>,
}

/// A custom (file-backed) theme as listed by `showLoadedResources`.
pub struct LoadedThemeResource {
    pub name: String,
    pub source_path: String,
    pub source_info: Option<SourceInfo>,
}

/// Everything `showLoadedResources` reads from the resource loader.
pub struct LoadedResources {
    pub cwd: String,
    pub context_paths: Vec<String>,
    pub skills: Vec<LoadedSkill>,
    pub skill_diagnostics: Vec<ResourceDiagnostic>,
    pub prompts: Vec<LoadedPrompt>,
    pub prompt_diagnostics: Vec<ResourceDiagnostic>,
    pub extensions: Vec<ScopedResource>,
    pub extension_diagnostics: Vec<ResourceDiagnostic>,
    pub themes: Vec<LoadedThemeResource>,
    pub theme_diagnostics: Vec<ResourceDiagnostic>,
}

/// The rebuilt loaded-resources container contents: every child in order, plus the sections that
/// follow the tools-expand toggle.
pub struct LoadedResourcesView {
    pub children: Vec<Rc<RefCell<dyn Component>>>,
    pub sections: Vec<Rc<RefCell<LoadedResourceSection>>>,
}

fn get_builtin_extension_display_name(extension_id: &str) -> &str {
    if extension_id == "todowrite" { "todo" } else { extension_id }
}

fn get_builtin_extension_name_from_path(path: &str) -> Option<String> {
    let captures = BUILTIN_PATH.captures(path)?;
    Some(get_builtin_extension_display_name(captures.get(1)?.as_str()).to_owned())
}

pub fn format_display_path(path: &str) -> String {
    if let Some(builtin_name) = get_builtin_extension_name_from_path(path) {
        return format!("builtin/{builtin_name}");
    }
    let home = maho_core::config::home_dir();
    match path.strip_prefix(home.as_str()) {
        Some(rest) => format!("~{rest}"),
        None => path.to_owned(),
    }
}

pub fn format_extension_display_path(path: &str) -> String {
    let result = format_display_path(path);
    let result = result.strip_suffix("/index.ts").map_or(result.clone(), str::to_owned);
    result.strip_suffix("/index.js").map_or(result.clone(), str::to_owned)
}

pub fn format_context_path(path: &str, cwd: &str) -> String {
    let options = maho_core::paths::PathInputOptions::default();
    let cwd = maho_core::paths::resolve_path(cwd, cwd, &options);
    let absolute_path = maho_core::paths::resolve_path(path, &cwd, &options);
    match maho_core::paths::get_cwd_relative_path(&absolute_path, &cwd) {
        Some(relative_path) => relative_path,
        None => format_display_path(&absolute_path),
    }
}

/// `path.posix.relative(from, to)` over already-normalized absolute paths.
fn posix_relative(from: &str, to: &str) -> String {
    let from_parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|part| !part.is_empty()).collect();
    let common = from_parts.iter().zip(&to_parts).take_while(|(left, right)| left == right).count();
    let mut parts: Vec<&str> = vec![".."; from_parts.len() - common];
    parts.extend(&to_parts[common..]);
    parts.join("/")
}

/// senpi `getShortPath`: a short path relative to the package root for display.
pub fn get_short_path(full_path: &str, source_info: Option<&SourceInfo>) -> String {
    let normalized_full_path = full_path.replace('\\', "/");
    if let Some(info) = source_info
        && let Some(base_dir) = info.base_dir.as_deref()
        && is_package_source_info(source_info)
    {
        let normalized_base_dir = base_dir.replace('\\', "/");
        if let Some(root) = NPM_ROOT.captures(&normalized_base_dir).and_then(|captures| captures.get(1))
            && normalized_full_path.starts_with(&format!("{}/", root.as_str()))
        {
            return posix_relative(&normalized_base_dir, &normalized_full_path);
        }
        let base = maho_core::paths::lexical_resolve(base_dir);
        let full = maho_core::paths::lexical_resolve(full_path);
        if let Ok(relative_path) = std::path::Path::new(&full).strip_prefix(std::path::Path::new(&base)) {
            let relative_path = relative_path.to_string_lossy().replace('\\', "/");
            if !relative_path.is_empty() && relative_path != "." {
                return relative_path;
            }
        }
    }
    let source = source_info.map_or("", |info| info.source.as_str());
    if source.starts_with("npm:")
        && let Some(path) = NPM_PATH.captures(&normalized_full_path).and_then(|captures| captures.get(2))
    {
        return path.as_str().to_owned();
    }
    if source.starts_with("git:")
        && let Some(path) = GIT_PATH.captures(&normalized_full_path).and_then(|captures| captures.get(1))
    {
        return path.as_str().to_owned();
    }
    format_display_path(full_path)
}

fn get_compact_path_label(resource_path: &str, source_info: Option<&SourceInfo>) -> String {
    let short_path = get_short_path(resource_path, source_info);
    let normalized_path = short_path.replace('\\', "/");
    normalized_path
        .split('/')
        .rfind(|segment| !segment.is_empty() && *segment != "~")
        .map_or(short_path.clone(), str::to_owned)
}

/// The `path` half of senpi `parseGitUrl` (`owner/repo`) for a package source string.
fn git_source_path(source: &str) -> Option<String> {
    let mut input = source.trim().strip_prefix("git:").unwrap_or(source.trim()).trim();
    for prefix in ["github:", "gitlab:", "bitbucket:", "gist:", "sourcehut:"] {
        if let Some(rest) = input.strip_prefix(prefix) {
            input = rest;
        }
    }
    let path = if let Some((_, rest)) = input.split_once("://") {
        rest.split_once('/')?.1
    } else if let Some(rest) = input.strip_prefix("git@") {
        rest.split_once(':')?.1
    } else if let Some((host, rest)) = input.split_once('/')
        && (host.contains('.') || host == "localhost")
    {
        rest
    } else {
        input
    };
    let path = path.split(['#', '@']).next().unwrap_or(path);
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    (path.split('/').count() >= 2).then(|| path.to_owned())
}

fn get_compact_package_source_label(source_info: Option<&SourceInfo>) -> String {
    let source = source_info.map_or("", |info| info.source.as_str());
    if let Some(name) = source.strip_prefix("npm:") {
        return if name.is_empty() { source.to_owned() } else { name.to_owned() };
    }
    if source.starts_with("git:")
        && let Some(path) = git_source_path(source)
    {
        return path;
    }
    source.to_owned()
}

/// `path.posix.parse(p)`'s `(dir, name)`.
fn posix_dir_and_name(path: &str) -> (&str, &str) {
    let (dir, base) = path.rsplit_once('/').unwrap_or(("", path));
    let name = match base.rfind('.') {
        Some(index) if index > 0 => &base[..index],
        _ => base,
    };
    (dir, name)
}

fn get_compact_extension_label(resource_path: &str, source_info: Option<&SourceInfo>) -> String {
    if !is_package_source_info(source_info) {
        return get_compact_path_label(resource_path, source_info);
    }
    let source_label = get_compact_package_source_label(source_info);
    if source_label.is_empty() {
        return get_compact_path_label(resource_path, source_info);
    }
    let short_path = get_short_path(resource_path, source_info).replace('\\', "/");
    let package_path = short_path.strip_prefix("extensions/").unwrap_or(&short_path);
    let (dir, name) = posix_dir_and_name(package_path);
    if name == "index" {
        return if dir.is_empty() || dir == "." { source_label } else { format!("{source_label}:{dir}") };
    }
    format!("{source_label}:{package_path}")
}

fn get_compact_display_path_segments(resource_path: &str) -> Vec<String> {
    format_display_path(resource_path)
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != "~")
        .map(str::to_owned)
        .collect()
}

fn get_compact_non_package_extension_label(resource_path: &str, index: usize, all_paths: &[(String, Vec<String>)]) -> String {
    let Some((_, segments)) = all_paths.get(index).filter(|(_, segments)| !segments.is_empty()) else {
        return get_compact_path_label(resource_path, None);
    };
    let tail = |segments: &[String], count: usize| segments[segments.len().saturating_sub(count)..].join("/");
    for segment_count in 1..=segments.len() {
        let candidate = tail(segments.as_slice(), segment_count);
        let is_unique = all_paths
            .iter()
            .enumerate()
            .all(|(item_index, (_, item_segments))| item_index == index || tail(item_segments.as_slice(), segment_count) != candidate);
        if is_unique {
            return candidate;
        }
    }
    segments.join("/")
}

fn get_compact_extension_labels(extensions: &[&ScopedResource]) -> Vec<String> {
    let non_package_extensions: Vec<(String, Vec<String>)> = extensions
        .iter()
        .filter(|extension| !is_package_source_info(extension.source_info.as_ref()))
        .map(|extension| {
            let mut segments = get_compact_display_path_segments(&extension.path);
            if segments.len() > 1 && segments.last().is_some_and(|last| last == "index.ts" || last == "index.js") {
                segments.pop();
            }
            (extension.path.clone(), segments)
        })
        .collect();
    extensions
        .iter()
        .map(|extension| {
            if is_package_source_info(extension.source_info.as_ref()) {
                return get_compact_extension_label(&extension.path, extension.source_info.as_ref());
            }
            match non_package_extensions.iter().position(|(path, _)| *path == extension.path) {
                Some(index) => get_compact_non_package_extension_label(&extension.path, index, &non_package_extensions),
                None => get_compact_path_label(&extension.path, extension.source_info.as_ref()),
            }
        })
        .collect()
}

fn find_source_info_for_path<'a>(path: &str, source_infos: &'a HashMap<String, SourceInfo>) -> Option<&'a SourceInfo> {
    if let Some(exact) = source_infos.get(path) {
        return Some(exact);
    }
    let mut current = path;
    while let Some(index) = current.rfind('/') {
        current = &current[..index];
        if let Some(parent) = source_infos.get(current) {
            return Some(parent);
        }
    }
    None
}

fn format_path_with_source(path: &str, source_info: Option<&SourceInfo>) -> String {
    match source_info {
        Some(info) => {
            let short_path = get_short_path(path, Some(info));
            let display = get_display_source_info(Some(info));
            let label_text = match display.scope_label {
                Some(scope_label) => format!("{} ({scope_label})", display.label),
                None => display.label,
            };
            format!("{label_text} {short_path}")
        }
        None => format_display_path(path),
    }
}

/// senpi `formatDiagnostics`: collisions grouped by name, then the remaining diagnostics.
pub fn format_diagnostics(diagnostics: &[ResourceDiagnostic], source_infos: &HashMap<String, SourceInfo>) -> Vec<NoticeLine> {
    let mut lines = Vec::new();
    let mut collisions: Vec<(String, Vec<&ResourceDiagnostic>)> = Vec::new();
    let mut other_diagnostics: Vec<&ResourceDiagnostic> = Vec::new();
    for diagnostic in diagnostics {
        match &diagnostic.collision {
            Some(collision) if diagnostic.diagnostic_type == ResourceDiagnosticType::Collision => {
                match collisions.iter_mut().find(|(name, _)| *name == collision.name) {
                    Some((_, list)) => list.push(diagnostic),
                    None => collisions.push((collision.name.clone(), vec![diagnostic])),
                }
            }
            _ => other_diagnostics.push(diagnostic),
        }
    }
    let warning = Some(ThemeColor::Warning);
    for (name, collision_list) in &collisions {
        let Some(first) = collision_list.first().and_then(|diagnostic| diagnostic.collision.as_ref()) else { continue };
        lines.push(NoticeLine { text: format!("  \"{name}\" collision:"), tone: warning });
        lines.push(NoticeLine {
            text: format!("    ✓ {}", format_path_with_source(&first.winner_path, find_source_info_for_path(&first.winner_path, source_infos))),
            tone: warning,
        });
        for collision in collision_list.iter().filter_map(|diagnostic| diagnostic.collision.as_ref()) {
            lines.push(NoticeLine {
                text: format!("    ✗ {} (skipped)", format_path_with_source(&collision.loser_path, find_source_info_for_path(&collision.loser_path, source_infos))),
                tone: warning,
            });
        }
    }
    for diagnostic in other_diagnostics {
        let tone = Some(if diagnostic.diagnostic_type == ResourceDiagnosticType::Error { ThemeColor::Error } else { ThemeColor::Warning });
        match &diagnostic.path {
            Some(path) => {
                let formatted_path = format_path_with_source(path, find_source_info_for_path(path, source_infos));
                lines.push(NoticeLine { text: format!("  {formatted_path}"), tone });
                lines.push(NoticeLine { text: format!("    {}", diagnostic.message), tone });
            }
            None => lines.push(NoticeLine { text: format!("  {}", diagnostic.message), tone }),
        }
    }
    lines
}

struct FormatWith<F: Fn(&ScopedResource) -> String, G: Fn(&ScopedResource, &str) -> String> {
    path: F,
    package_path: G,
}

impl<F: Fn(&ScopedResource) -> String, G: Fn(&ScopedResource, &str) -> String> ResourceScopeGroupFormat for FormatWith<F, G> {
    fn format_path(&self, item: &ScopedResource) -> String {
        (self.path)(item)
    }
    fn format_package_path(&self, item: &ScopedResource, source: &str) -> String {
        (self.package_path)(item, source)
    }
}

fn format_compact_list(items: Vec<String>, sort: bool, theme: &Theme) -> String {
    let mut labels: Vec<String> = items.into_iter().map(|item| item.trim().to_owned()).filter(|item| !item.is_empty()).collect();
    if labels.is_empty() {
        return String::new();
    }
    if sort {
        let collator = icu_collator::Collator::try_new(Default::default(), Default::default()).expect("compiled collation data is available");
        labels.sort_by(|left, right| collator.compare(left, right));
    }
    theme.fg(ThemeColor::Dim, &format!("  {}", labels.join(", ")))
}

/// senpi `showLoadedResources` body: the sections (when `show_listing`) and diagnostic notices
/// (when `show_diagnostics`) for the loaded-resources container.
pub fn build_loaded_resources(
    resources: &LoadedResources,
    show_listing: bool,
    show_diagnostics: bool,
    startup_expanded: bool,
    tool_output_expanded: bool,
    theme: &Theme,
) -> LoadedResourcesView {
    let mut view = LoadedResourcesView { children: Vec::new(), sections: Vec::new() };
    if !show_listing && !show_diagnostics {
        return view;
    }
    let section_header = |name: &str, color: ThemeColor| theme.fg(color, &format!("[{name}]"));
    let add_loaded_section = |view: &mut LoadedResourcesView, name: &str, collapsed_body: String, expanded_body: String| {
        let header = section_header(name, ThemeColor::MdHeading);
        let collapsed = if collapsed_body.is_empty() { String::new() } else { format!("{header}\n{collapsed_body}") };
        let section = Rc::new(RefCell::new(LoadedResourceSection::new(collapsed, format!("{header}\n{expanded_body}"), startup_expanded)));
        view.children.push(section.clone());
        view.sections.push(section);
    };

    let mut source_infos: HashMap<String, SourceInfo> = HashMap::new();
    for extension in &resources.extensions {
        if let Some(info) = &extension.source_info { source_infos.insert(extension.path.clone(), info.clone()); }
    }
    for skill in &resources.skills {
        if let Some(info) = &skill.source_info { source_infos.insert(skill.file_path.clone(), info.clone()); }
    }
    for prompt in &resources.prompts {
        if let Some(info) = &prompt.source_info { source_infos.insert(prompt.file_path.clone(), info.clone()); }
    }
    for loaded_theme in &resources.themes {
        if let Some(info) = &loaded_theme.source_info { source_infos.insert(loaded_theme.source_path.clone(), info.clone()); }
    }

    if show_listing {
        if !resources.context_paths.is_empty() {
            view.children.push(Rc::new(RefCell::new(Spacer::new(1))));
            let context_list = resources.context_paths.iter().map(|path| theme.fg(ThemeColor::Dim, &format!("  {}", format_display_path(path)))).collect::<Vec<_>>().join("\n");
            let context_compact_list = format_compact_list(resources.context_paths.iter().map(|path| format_context_path(path, &resources.cwd)).collect(), false, theme);
            add_loaded_section(&mut view, "Context", context_compact_list, context_list);
        }

        if !resources.skills.is_empty() {
            let groups = build_resource_scope_groups(&resources.skills.iter().map(|skill| ScopedResource { path: skill.file_path.clone(), source_info: skill.source_info.clone() }).collect::<Vec<_>>());
            let skill_list = format_resource_scope_groups(&groups, &FormatWith {
                path: |item: &ScopedResource| format_display_path(&item.path),
                package_path: |item: &ScopedResource, _source: &str| get_short_path(&item.path, item.source_info.as_ref()),
            }, theme);
            let skill_compact_list = format_compact_list(resources.skills.iter().filter(|skill| !is_system_resource(skill.source_info.as_ref())).map(|skill| skill.name.clone()).collect(), true, theme);
            add_loaded_section(&mut view, "Skills", skill_compact_list, skill_list);
        }

        if !resources.prompts.is_empty() {
            let groups = build_resource_scope_groups(&resources.prompts.iter().map(|prompt| ScopedResource { path: prompt.file_path.clone(), source_info: prompt.source_info.clone() }).collect::<Vec<_>>());
            let template_by_path: HashMap<&str, &LoadedPrompt> = resources.prompts.iter().map(|prompt| (prompt.file_path.as_str(), prompt)).collect();
            let format_template = |item: &ScopedResource| match template_by_path.get(item.path.as_str()) {
                Some(template) => format!("/{}", template.name),
                None => format_display_path(&item.path),
            };
            let template_list = format_resource_scope_groups(&groups, &FormatWith { path: &format_template, package_path: |item: &ScopedResource, _source: &str| format_template(item) }, theme);
            let prompt_compact_list = format_compact_list(resources.prompts.iter().filter(|prompt| !is_system_resource(prompt.source_info.as_ref())).map(|prompt| format!("/{}", prompt.name)).collect(), true, theme);
            add_loaded_section(&mut view, "Prompts", prompt_compact_list, template_list);
        }

        if !resources.extensions.is_empty() {
            let groups = build_resource_scope_groups(&resources.extensions);
            let extension_list = format_resource_scope_groups(&groups, &FormatWith {
                path: |item: &ScopedResource| format_extension_display_path(&item.path),
                package_path: |item: &ScopedResource, _source: &str| format_extension_display_path(&get_short_path(&item.path, item.source_info.as_ref())),
            }, theme);
            let visible: Vec<&ScopedResource> = resources.extensions.iter().filter(|extension| !is_system_resource(extension.source_info.as_ref())).collect();
            let extension_compact_list = format_compact_list(get_compact_extension_labels(&visible), true, theme);
            add_loaded_section(&mut view, "Extensions", extension_compact_list, extension_list);
        }

        if !resources.themes.is_empty() {
            let groups = build_resource_scope_groups(&resources.themes.iter().map(|loaded_theme| ScopedResource { path: loaded_theme.source_path.clone(), source_info: loaded_theme.source_info.clone() }).collect::<Vec<_>>());
            let theme_list = format_resource_scope_groups(&groups, &FormatWith {
                path: |item: &ScopedResource| format_display_path(&item.path),
                package_path: |item: &ScopedResource, _source: &str| get_short_path(&item.path, item.source_info.as_ref()),
            }, theme);
            let theme_compact_list = format_compact_list(resources.themes.iter().filter(|loaded_theme| !is_system_resource(loaded_theme.source_info.as_ref())).map(|loaded_theme| loaded_theme.name.clone()).collect(), true, theme);
            add_loaded_section(&mut view, "Themes", theme_compact_list, theme_list);
        }
    }

    if show_diagnostics {
        let add_diagnostic_notice = |view: &mut LoadedResourcesView, title: &str, diagnostics: &[ResourceDiagnostic]| {
            if diagnostics.is_empty() { return; }
            let spec = NoticeSpec {
                title: title.to_owned(),
                tone: Some(ThemeColor::Warning),
                why: "Conflicting or invalid loaded resources were found.".to_owned(),
                extra: format_diagnostics(diagnostics, &source_infos),
                expanded_line: None,
            };
            view.children.push(build_notice_box(&spec, tool_output_expanded, theme));
            view.children.push(Rc::new(RefCell::new(Spacer::new(1))));
        };
        add_diagnostic_notice(&mut view, "Skill conflicts", &resources.skill_diagnostics);
        add_diagnostic_notice(&mut view, "Prompt conflicts", &resources.prompt_diagnostics);
        add_diagnostic_notice(&mut view, "Extension issues", &resources.extension_diagnostics);
        add_diagnostic_notice(&mut view, "Theme conflicts", &resources.theme_diagnostics);
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::source_info::{SourceOrigin, SourceScope};

    fn info(source: &str, scope: SourceScope, base_dir: Option<&str>) -> SourceInfo {
        SourceInfo { path: String::new(), source: source.to_owned(), scope, origin: SourceOrigin::Package, base_dir: base_dir.map(str::to_owned) }
    }

    #[test]
    fn display_path_names_builtins_and_strips_index_files() {
        assert_eq!(format_display_path("<builtin:todowrite>"), "builtin/todo");
        assert_eq!(format_extension_display_path("/opt/ext/foo/index.ts"), "/opt/ext/foo");
    }

    #[test]
    fn short_path_is_relative_to_the_package_base_dir() {
        let npm = info("npm:pkg", SourceScope::User, Some("/root/node_modules/pkg"));
        assert_eq!(get_short_path("/root/node_modules/pkg/skills/a/SKILL.md", Some(&npm)), "skills/a/SKILL.md");
        let git = info("git:github.com/o/r", SourceScope::User, None);
        assert_eq!(get_short_path("/x/git/github.com/o/r/extensions/e.ts", Some(&git)), "r/extensions/e.ts");
    }

    #[test]
    fn compact_extension_labels_disambiguate_local_paths_and_name_packages() {
        let first = ScopedResource { path: "/a/one/tool/index.ts".into(), source_info: None };
        let second = ScopedResource { path: "/a/two/tool/index.ts".into(), source_info: None };
        let package = ScopedResource { path: "/r/node_modules/pkg/extensions/index.ts".into(), source_info: Some(info("npm:pkg", SourceScope::User, Some("/r/node_modules/pkg"))) };
        assert_eq!(get_compact_extension_labels(&[&first, &second, &package]), vec!["one/tool", "two/tool", "pkg"]);
    }

    #[test]
    fn collision_diagnostics_group_by_name() {
        use maho_core::diagnostics::{ResourceCollision, ResourceCollisionType};
        let collision = |loser: &str| ResourceDiagnostic {
            category: None,
            diagnostic_type: ResourceDiagnosticType::Collision,
            message: String::new(),
            path: None,
            collision: Some(ResourceCollision { resource_type: ResourceCollisionType::Skill, name: "dup".into(), winner_path: "/w".into(), loser_path: loser.into(), winner_source: None, loser_source: None }),
        };
        let lines = format_diagnostics(&[collision("/l1"), collision("/l2")], &HashMap::new());
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, vec!["  \"dup\" collision:", "    ✓ /w", "    ✗ /l1 (skipped)", "    ✗ /l2 (skipped)"]);
    }

    #[test]
    fn compact_section_is_hidden_when_only_system_resources_load() {
        let theme = Theme::builtin("dark", crate::theme::ColorMode::Color256).expect("builtin theme");
        let resources = LoadedResources {
            cwd: "/w".into(), context_paths: Vec::new(),
            skills: vec![LoadedSkill { name: "sys".into(), file_path: "/s/SKILL.md".into(), source_info: Some(info("local", SourceScope::System, None)) }],
            skill_diagnostics: Vec::new(), prompts: Vec::new(), prompt_diagnostics: Vec::new(),
            extensions: Vec::new(), extension_diagnostics: Vec::new(), themes: Vec::new(), theme_diagnostics: Vec::new(),
        };
        let view = build_loaded_resources(&resources, true, true, false, false, &theme);
        assert_eq!(view.sections.len(), 1);
        assert!(view.sections[0].borrow_mut().render(80).is_empty());
        view.sections[0].borrow_mut().set_expanded(true);
        assert!(!view.sections[0].borrow_mut().render(80).is_empty());
    }
}
