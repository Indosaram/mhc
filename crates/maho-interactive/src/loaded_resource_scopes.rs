use maho_core::source_info::{SourceInfo, SourceScope};
use crate::theme::{Theme, ThemeColor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceScopeGroup { Project, User, Path, System }

impl ResourceScopeGroup {
    pub const fn as_str(self) -> &'static str {
        match self { Self::Project => "project", Self::User => "user", Self::Path => "path", Self::System => "system" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedResource {
    pub path: String,
    pub source_info: Option<SourceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceScopeGroups {
    pub scope: ResourceScopeGroup,
    pub paths: Vec<ScopedResource>,
    pub packages: Vec<(String, Vec<ScopedResource>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySourceInfo {
    pub label: String,
    pub scope_label: Option<&'static str>,
    pub color: ThemeColor,
}

pub fn is_system_resource(info: Option<&SourceInfo>) -> bool {
    info.is_some_and(|info| info.scope == SourceScope::System)
}

pub fn is_package_source_info(info: Option<&SourceInfo>) -> bool {
    info.is_some_and(|info| info.source.starts_with("npm:") || info.source.starts_with("git:"))
}

pub fn get_resource_scope_group(info: Option<&SourceInfo>) -> ResourceScopeGroup {
    let scope = info.map_or(SourceScope::Project, |info| info.scope);
    if scope == SourceScope::System { return ResourceScopeGroup::System; }
    if info.is_some_and(|info| info.source == "cli") { return ResourceScopeGroup::Path; }
    match scope {
        SourceScope::Temporary => ResourceScopeGroup::Path,
        SourceScope::User => ResourceScopeGroup::User,
        SourceScope::Project => ResourceScopeGroup::Project,
        SourceScope::System => ResourceScopeGroup::System,
    }
}

pub fn build_resource_scope_groups(items: &[ScopedResource]) -> Vec<ResourceScopeGroups> {
    let mut groups: Vec<_> = [ResourceScopeGroup::Project, ResourceScopeGroup::User, ResourceScopeGroup::Path, ResourceScopeGroup::System]
        .into_iter().map(|scope| ResourceScopeGroups { scope, paths: Vec::new(), packages: Vec::new() }).collect();
    for item in items {
        let scope = get_resource_scope_group(item.source_info.as_ref());
        if let Some(group) = groups.iter_mut().find(|group| group.scope == scope) {
            if is_package_source_info(item.source_info.as_ref()) {
                let source = item.source_info.as_ref().map_or("local", |info| info.source.as_str());
                if let Some((_, resources)) = group.packages.iter_mut().find(|(key, _)| key == source) {
                    resources.push(item.clone());
                } else {
                    group.packages.push((source.into(), vec![item.clone()]));
                }
            } else {
                group.paths.push(item.clone());
            }
        }
    }
    groups.retain(|group| !group.paths.is_empty() || !group.packages.is_empty());
    groups
}

pub trait ResourceScopeGroupFormat {
    fn format_path(&self, item: &ScopedResource) -> String;
    fn format_package_path(&self, item: &ScopedResource, source: &str) -> String;
}

pub fn format_resource_scope_groups(groups: &[ResourceScopeGroups], format: &impl ResourceScopeGroupFormat, theme: &Theme) -> String {
    let mut lines = Vec::new();
    for group in groups {
        lines.push(format!("  {}", theme.fg(ThemeColor::Accent, group.scope.as_str())));
        let mut paths: Vec<_> = group.paths.iter().collect();
        paths.sort_by(|a, b| a.path.cmp(&b.path));
        for item in paths { lines.push(theme.fg(ThemeColor::Dim, &format!("    {}", format.format_path(item)))); }
        let mut packages: Vec<_> = group.packages.iter().collect();
        packages.sort_by(|a, b| a.0.cmp(&b.0));
        for (source, items) in packages {
            lines.push(format!("    {}", theme.fg(ThemeColor::MdLink, source)));
            let mut paths: Vec<_> = items.iter().collect();
            paths.sort_by(|a, b| a.path.cmp(&b.path));
            for item in paths { lines.push(theme.fg(ThemeColor::Dim, &format!("      {}", format.format_package_path(item, source)))); }
        }
    }
    lines.join("\n")
}

pub const fn get_scope_autocomplete_tag(scope: SourceScope) -> &'static str {
    match scope { SourceScope::User => "u", SourceScope::Project => "p", SourceScope::Temporary => "t", SourceScope::System => "s" }
}

pub fn get_display_source_info(info: Option<&SourceInfo>) -> DisplaySourceInfo {
    let source = info.map_or("local", |info| info.source.as_str());
    let scope = info.map_or(SourceScope::Project, |info| info.scope);
    let (label, scope_label, color) = if scope == SourceScope::System {
        ("system", None, ThemeColor::Muted)
    } else if source == "local" {
        match scope {
            SourceScope::User => ("user", None, ThemeColor::Muted),
            SourceScope::Project => ("project", None, ThemeColor::Muted),
            SourceScope::Temporary => ("path", Some("temp"), ThemeColor::Muted),
            SourceScope::System => ("system", None, ThemeColor::Muted),
        }
    } else if source == "cli" {
        ("path", (scope == SourceScope::Temporary).then_some("temp"), ThemeColor::Muted)
    } else {
        let label = match scope { SourceScope::User => "user", SourceScope::Project => "project", SourceScope::Temporary | SourceScope::System => "temp" };
        (source, Some(label), ThemeColor::Accent)
    };
    DisplaySourceInfo { label: label.into(), scope_label, color }
}
