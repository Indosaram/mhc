//! Port of components/config-selector.ts.
//!
//! TUI component for managing package resources (enable/disable). senpi's SettingsManager write
//! API (getGlobalSettings, getProjectSettings, setExtensionPaths, setPackages, ...) is owned by
//! plan todo 21, so this port reaches it through the ConfigSettingsHost trait; todo 35 implements
//! it over the real manager.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use maho_core::package_manager::{PathMetadata, ResolvedPaths, ResolvedResource};
use maho_core::source_info::SourceScope;
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::keybindings::get_keybindings;
use maho_tui::keys::matches_key;
use maho_tui::tui::{Component, Focusable};
use maho_tui::utils::{truncate_to_width, visible_width};
use serde_json::Value;

use super::keybinding_hints::{key_hint, raw_key_hint};
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeColor};

/// The subset of senpi's SettingsManager the config selector writes through.
pub trait ConfigSettingsHost {
    fn get_global_settings(&self) -> Value;
    fn get_project_settings(&self) -> Value;
    fn set_packages(&mut self, packages: &[Value]);
    fn set_project_packages(&mut self, packages: &[Value]);
    fn set_extension_paths(&mut self, paths: &[String]);
    fn set_project_extension_paths(&mut self, paths: &[String]);
    fn set_skill_paths(&mut self, paths: &[String]);
    fn set_project_skill_paths(&mut self, paths: &[String]);
    fn set_prompt_template_paths(&mut self, paths: &[String]);
    fn set_project_prompt_template_paths(&mut self, paths: &[String]);
    fn set_theme_paths(&mut self, paths: &[String]);
    fn set_project_theme_paths(&mut self, paths: &[String]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

impl ResourceType {
    pub const ALL: [Self; 4] = [Self::Extensions, Self::Skills, Self::Prompts, Self::Themes];

    pub fn key(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Extensions => "Extensions",
            Self::Skills => "Skills",
            Self::Prompts => "Prompts",
            Self::Themes => "Themes",
        }
    }

    fn order(self) -> usize {
        match self {
            Self::Extensions => 0,
            Self::Skills => 1,
            Self::Prompts => 2,
            Self::Themes => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigWriteScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScope {
    User,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectOverrideState {
    Inherit,
    Load,
    Unload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceItem {
    pub path: String,
    pub enabled: bool,
    pub metadata: PathMetadata,
    pub resource_type: ResourceType,
    pub display_name: String,
    pub group_key: String,
    pub subgroup_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSubgroup {
    pub resource_type: ResourceType,
    pub label: String,
    pub items: Vec<ResourceItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceGroup {
    pub key: String,
    pub label: String,
    pub scope: SourceScope,
    pub origin: maho_core::source_info::SourceOrigin,
    pub source: String,
    pub subgroups: Vec<ResourceSubgroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlatEntry {
    Group(ResourceGroup),
    Subgroup { subgroup: ResourceSubgroup, group: ResourceGroup },
    Item(ResourceItem),
}

pub fn format_base_dir(base_dir: &str, home_dir: &str) -> String {
    let display = if base_dir == home_dir {
        "~".to_owned()
    } else if let Some(rest) = base_dir.strip_prefix(home_dir) {
        format!("~{}", rest.replace('\\', "/"))
    } else {
        base_dir.replace('\\', "/")
    };
    if display.ends_with('/') { display } else { format!("{display}/") }
}

/// senpi's getGroupLabel.
pub fn get_group_label(metadata: &PathMetadata, agent_dir: &str, home_dir: &str, config_dir_name: &str) -> String {
    if metadata.origin == maho_core::source_info::SourceOrigin::Package {
        return format!("{} ({})", metadata.source, metadata.scope.as_str());
    }
    if metadata.source == "auto" {
        if let Some(base_dir) = &metadata.base_dir {
            return match metadata.scope {
                SourceScope::User => format!("User ({})", format_base_dir(base_dir, home_dir)),
                _ => format!("Project ({})", format_base_dir(base_dir, home_dir)),
            };
        }
        return match metadata.scope {
            SourceScope::User => format!("User ({})", format_base_dir(agent_dir, home_dir)),
            _ => format!("Project ({config_dir_name}/)"),
        };
    }
    match metadata.scope {
        SourceScope::User => "User settings".to_owned(),
        _ => "Project settings".to_owned(),
    }
}

/// senpi's buildGroups.
pub fn build_groups(
    resolved: &ResolvedPaths,
    agent_dir: &str,
    home_dir: &str,
    config_dir_name: &str,
) -> Vec<ResourceGroup> {
    let mut group_map: BTreeMap<String, ResourceGroup> = BTreeMap::new();

    let mut add_to_group = |resources: &[ResolvedResource], resource_type: ResourceType| {
        for resource in resources {
            let metadata = &resource.metadata;
            let group_key = format!(
                "{}:{}:{}:{}",
                metadata.origin.as_str(),
                metadata.scope.as_str(),
                metadata.source,
                metadata.base_dir.clone().unwrap_or_default()
            );
            let group = group_map.entry(group_key.clone()).or_insert_with(|| ResourceGroup {
                key: group_key.clone(),
                label: get_group_label(metadata, agent_dir, home_dir, config_dir_name),
                scope: metadata.scope,
                origin: metadata.origin,
                source: metadata.source.clone(),
                subgroups: Vec::new(),
            });
            let subgroup_key = format!("{group_key}:{}", resource_type.key());
            if !group.subgroups.iter().any(|sg| sg.resource_type == resource_type) {
                group.subgroups.push(ResourceSubgroup {
                    resource_type,
                    label: resource_type.label().to_owned(),
                    items: Vec::new(),
                });
            }
            let file_name = Path::new(&resource.path)
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let parent_folder = Path::new(&resource.path)
                .parent()
                .and_then(Path::file_name)
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let display_name = if resource_type == ResourceType::Extensions && parent_folder != "extensions" {
                format!("{parent_folder}/{file_name}")
            } else if resource_type == ResourceType::Skills && file_name == "SKILL.md" {
                parent_folder
            } else {
                file_name
            };
            let subgroup = group
                .subgroups
                .iter_mut()
                .find(|sg| sg.resource_type == resource_type)
                .expect("subgroup inserted above");
            subgroup.items.push(ResourceItem {
                path: resource.path.clone(),
                enabled: resource.enabled,
                metadata: metadata.clone(),
                resource_type,
                display_name,
                group_key: group_key.clone(),
                subgroup_key,
            });
        }
    };

    add_to_group(&resolved.extensions, ResourceType::Extensions);
    add_to_group(&resolved.skills, ResourceType::Skills);
    add_to_group(&resolved.prompts, ResourceType::Prompts);
    add_to_group(&resolved.themes, ResourceType::Themes);

    let mut groups: Vec<ResourceGroup> = group_map.into_values().collect();
    groups.sort_by(|a, b| {
        if a.origin != b.origin {
            return if a.origin == maho_core::source_info::SourceOrigin::Package {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        if a.scope != b.scope {
            return if a.scope == SourceScope::User {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        a.source.cmp(&b.source)
    });
    for group in &mut groups {
        group.subgroups.sort_by_key(|sg| sg.resource_type.order());
        for subgroup in &mut group.subgroups {
            subgroup.items.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        }
    }
    groups
}

/// senpi's ConfigSelectorHeader.
pub struct ConfigSelectorHeader {
    theme: Theme,
    write_scope: ConfigWriteScope,
    project_mode_available: bool,
    config_dir_name: String,
}

impl ConfigSelectorHeader {
    pub fn new(
        theme: &Theme,
        write_scope: ConfigWriteScope,
        project_mode_available: bool,
        config_dir_name: &str,
    ) -> Self {
        Self {
            theme: theme.clone(),
            write_scope,
            project_mode_available,
            config_dir_name: config_dir_name.to_owned(),
        }
    }

    pub fn set_write_scope(&mut self, write_scope: ConfigWriteScope) {
        self.write_scope = write_scope;
    }
}

impl Component for ConfigSelectorHeader {
    fn render(&mut self, width: usize) -> Vec<String> {
        let title = self.theme.bold(match self.write_scope {
            ConfigWriteScope::Project => "Project Local Resources",
            ConfigWriteScope::Global => "Global Resources",
        });
        let sep = self.theme.fg(ThemeColor::Muted, " · ");
        let switch_hint = if self.project_mode_available {
            format!("{}{sep}", key_hint("tui.input.tab", "switch mode", &self.theme))
        } else {
            String::new()
        };
        let action_hint = match self.write_scope {
            ConfigWriteScope::Project => raw_key_hint("space", "cycle inherit/+/-", &self.theme),
            ConfigWriteScope::Global => raw_key_hint("space", "toggle", &self.theme),
        };
        let hint = format!(
            "{switch_hint}{action_hint}{sep}{}",
            raw_key_hint("esc", "close", &self.theme)
        );
        let spacing = width
            .saturating_sub(visible_width(&title))
            .saturating_sub(visible_width(&hint))
            .max(1);
        let scope_hint = match self.write_scope {
            ConfigWriteScope::Project => self.theme.fg(
                ThemeColor::Muted,
                &format!(
                    "{}/settings.json · inherited global resources are dimmed",
                    self.config_dir_name
                ),
            ),
            ConfigWriteScope::Global => self
                .theme
                .fg(ThemeColor::Muted, &format!("~/{}/agent/settings.json", self.config_dir_name)),
        };
        vec![
            truncate_to_width(&format!("{title}{}{hint}", " ".repeat(spacing)), width, "", false),
            truncate_to_width(&scope_hint, width, "", false),
        ]
    }

    fn invalidate(&mut self) {}
}

fn settings_array(settings: &Value, key: &str) -> Vec<String> {
    settings
        .get(key)
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn pattern_entry_target(entry: &str) -> &str {
    entry
        .strip_prefix('!')
        .or_else(|| entry.strip_prefix('+'))
        .or_else(|| entry.strip_prefix('-'))
        .unwrap_or(entry)
}

fn package_source_string(package: &Value) -> String {
    match package {
        Value::String(source) => source.clone(),
        other => other.get("source").and_then(Value::as_str).unwrap_or_default().to_owned(),
    }
}

/// senpi's onToggle(item, newEnabled).
pub type ResourceToggleHandler = Box<dyn FnMut(&ResourceItem, bool)>;
pub type VoidHandler = Box<dyn FnMut()>;

/// senpi's ResourceList.
pub struct ResourceList {
    theme: Theme,
    groups_by_scope: BTreeMap<ConfigWriteScope, Vec<ResourceGroup>>,
    flat_items: Vec<FlatEntry>,
    filtered_items: Vec<FlatEntry>,
    selected_index: usize,
    search_input: Input,
    max_visible: usize,
    settings: Box<dyn ConfigSettingsHost>,
    cwd: String,
    agent_dir: String,
    config_dir_name: String,
    write_scope: ConfigWriteScope,
    inherited_enabled_by_key: BTreeMap<String, bool>,
    focused: bool,
    pub on_cancel: Option<VoidHandler>,
    pub on_exit: Option<VoidHandler>,
    pub on_toggle: Option<ResourceToggleHandler>,
    pub on_switch_mode: Option<VoidHandler>,
}

impl ResourceList {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        groups_by_scope: BTreeMap<ConfigWriteScope, Vec<ResourceGroup>>,
        settings: Box<dyn ConfigSettingsHost>,
        cwd: &str,
        agent_dir: &str,
        config_dir_name: &str,
        terminal_height: Option<usize>,
        write_scope: ConfigWriteScope,
    ) -> Self {
        let inherited_enabled_by_key = build_inherited_enabled_map(
            groups_by_scope.get(&ConfigWriteScope::Global).map_or(&[][..], Vec::as_slice),
        );
        // 8 lines of chrome: top spacer + top border + spacer + header (2 lines) + spacer +
        // bottom spacer + bottom border.
        let chrome = 8;
        let max_visible = (terminal_height.unwrap_or(24)).saturating_sub(chrome).max(5);
        let mut list = Self {
            theme: theme.clone(),
            groups_by_scope,
            flat_items: Vec::new(),
            filtered_items: Vec::new(),
            selected_index: 0,
            search_input: Input::new(InputOptions::default()),
            max_visible,
            settings,
            cwd: cwd.to_owned(),
            agent_dir: agent_dir.to_owned(),
            config_dir_name: config_dir_name.to_owned(),
            write_scope,
            inherited_enabled_by_key,
            focused: false,
            on_cancel: None,
            on_exit: None,
            on_toggle: None,
            on_switch_mode: None,
        };
        list.build_flat_list();
        list.filtered_items = list.flat_items.clone();
        list
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn filtered_items(&self) -> &[FlatEntry] {
        &self.filtered_items
    }

    pub fn set_write_scope(&mut self, write_scope: ConfigWriteScope) {
        self.write_scope = write_scope;
        self.build_flat_list();
        let query = self.search_input.get_value().to_owned();
        self.filter_items(&query);
    }

    fn groups(&self) -> &[ResourceGroup] {
        self.groups_by_scope.get(&self.write_scope).map_or(&[][..], Vec::as_slice)
    }

    fn groups_mut(&mut self) -> &mut Vec<ResourceGroup> {
        self.groups_by_scope.entry(self.write_scope).or_default()
    }

    fn build_flat_list(&mut self) {
        self.flat_items = Vec::new();
        let groups = self.groups().to_vec();
        for group in groups {
            self.flat_items.push(FlatEntry::Group(group.clone()));
            for subgroup in &group.subgroups {
                self.flat_items.push(FlatEntry::Subgroup { subgroup: subgroup.clone(), group: group.clone() });
                for item in &subgroup.items {
                    self.flat_items.push(FlatEntry::Item(item.clone()));
                }
            }
        }
        self.selected_index = self
            .flat_items
            .iter()
            .position(|entry| matches!(entry, FlatEntry::Item(_)))
            .unwrap_or(0);
    }

    fn find_next_item(&self, from_index: usize, direction: i64) -> usize {
        let mut index = from_index as i64 + direction;
        while index >= 0 && (index as usize) < self.filtered_items.len() {
            if matches!(self.filtered_items[index as usize], FlatEntry::Item(_)) {
                return index as usize;
            }
            index += direction;
        }
        from_index
    }

    fn filter_items(&mut self, query: &str) {
        if query.trim().is_empty() {
            self.filtered_items = self.flat_items.clone();
            self.select_first_item();
            return;
        }
        let lower_query = query.to_lowercase();
        let mut matching_items: BTreeSet<String> = BTreeSet::new();
        for entry in &self.flat_items {
            if let FlatEntry::Item(item) = entry
                && (item.display_name.to_lowercase().contains(&lower_query)
                    || item.resource_type.key().contains(&lower_query)
                    || item.path.to_lowercase().contains(&lower_query))
            {
                matching_items.insert(item.path.clone());
            }
        }
        let mut matching_subgroups: BTreeSet<String> = BTreeSet::new();
        let mut matching_groups: BTreeSet<String> = BTreeSet::new();
        for group in self.groups() {
            for subgroup in &group.subgroups {
                for item in &subgroup.items {
                    if matching_items.contains(&item.path) {
                        matching_subgroups.insert(subgroup_label_key(group, subgroup));
                        matching_groups.insert(group.key.clone());
                    }
                }
            }
        }

        self.filtered_items = self
            .flat_items
            .iter()
            .filter(|entry| match entry {
                FlatEntry::Group(group) => matching_groups.contains(&group.key),
                FlatEntry::Subgroup { subgroup, group } => matching_subgroups.contains(&subgroup_label_key(group, subgroup)),
                FlatEntry::Item(item) => matching_items.contains(&item.path),
            })
            .cloned()
            .collect();
        self.select_first_item();
    }

    fn select_first_item(&mut self) {
        self.selected_index = self
            .filtered_items
            .iter()
            .position(|entry| matches!(entry, FlatEntry::Item(_)))
            .unwrap_or(0);
    }

    pub fn update_item(&mut self, item: &ResourceItem, enabled: bool) {
        for group in self.groups_mut() {
            for subgroup in &mut group.subgroups {
                if let Some(found) = subgroup
                    .items
                    .iter_mut()
                    .find(|i| i.path == item.path && i.resource_type == item.resource_type)
                {
                    found.enabled = enabled;
                }
            }
        }
    }

    fn render_checkbox(&self, item: &ResourceItem) -> String {
        if self.write_scope == ConfigWriteScope::Project {
            return match self.get_project_override_state(item) {
                ProjectOverrideState::Load => self.theme.fg(ThemeColor::Success, "[+]"),
                ProjectOverrideState::Unload => self.theme.fg(ThemeColor::Warning, "[-]"),
                ProjectOverrideState::Inherit => {
                    self.theme.fg(ThemeColor::Dim, if item.enabled { "[x]" } else { "[ ]" })
                }
            };
        }
        if item.enabled {
            self.theme.fg(ThemeColor::Success, "[x]")
        } else {
            self.theme.fg(ThemeColor::Dim, "[ ]")
        }
    }

    fn get_item_suffix(&self, item: &ResourceItem) -> String {
        if self.write_scope != ConfigWriteScope::Project {
            return String::new();
        }
        match self.get_project_override_state(item) {
            ProjectOverrideState::Load => self.theme.fg(ThemeColor::Muted, "  project load"),
            ProjectOverrideState::Unload => self.theme.fg(ThemeColor::Muted, "  project unload"),
            ProjectOverrideState::Inherit => {
                if self.is_inherited_global_item(item) {
                    self.theme.fg(ThemeColor::Dim, "  inherited global")
                } else {
                    String::new()
                }
            }
        }
    }

    fn is_dimmed_item(&self, item: &ResourceItem) -> bool {
        self.write_scope == ConfigWriteScope::Project
            && self.is_inherited_global_item(item)
            && self.get_project_override_state(item) == ProjectOverrideState::Inherit
    }

    fn toggle_resource(&mut self, item: &ResourceItem) -> Option<bool> {
        if self.write_scope == ConfigWriteScope::Project {
            let state = self.get_next_override_state(item);
            if !self.set_project_resource_override(item, state) {
                return None;
            }
            return Some(match state {
                ProjectOverrideState::Inherit => self.get_inherited_enabled(item),
                ProjectOverrideState::Load => true,
                ProjectOverrideState::Unload => false,
            });
        }
        let enabled = !item.enabled;
        if item.metadata.origin == maho_core::source_info::SourceOrigin::TopLevel {
            self.toggle_top_level_resource(item, enabled);
        } else {
            self.toggle_package_resource(item, enabled);
        }
        Some(enabled)
    }

    fn toggle_top_level_resource(&mut self, item: &ResourceItem, enabled: bool) {
        let scope = if item.metadata.scope == SourceScope::Project {
            SettingsScope::Project
        } else {
            SettingsScope::User
        };
        let settings = match scope {
            SettingsScope::Project => self.settings.get_project_settings(),
            SettingsScope::User => self.settings.get_global_settings(),
        };
        let current = settings_array(&settings, item.resource_type.key());
        let pattern = self.get_resource_pattern(item);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| pattern_entry_target(entry) != pattern)
            .collect();
        updated.push(format!("{}{pattern}", if enabled { "+" } else { "-" }));
        self.write_top_level_paths(scope, item.resource_type, &updated);
    }

    fn write_top_level_paths(&mut self, scope: SettingsScope, resource_type: ResourceType, paths: &[String]) {
        match (scope, resource_type) {
            (SettingsScope::Project, ResourceType::Extensions) => self.settings.set_project_extension_paths(paths),
            (SettingsScope::Project, ResourceType::Skills) => self.settings.set_project_skill_paths(paths),
            (SettingsScope::Project, ResourceType::Prompts) => self.settings.set_project_prompt_template_paths(paths),
            (SettingsScope::Project, ResourceType::Themes) => self.settings.set_project_theme_paths(paths),
            (SettingsScope::User, ResourceType::Extensions) => self.settings.set_extension_paths(paths),
            (SettingsScope::User, ResourceType::Skills) => self.settings.set_skill_paths(paths),
            (SettingsScope::User, ResourceType::Prompts) => self.settings.set_prompt_template_paths(paths),
            (SettingsScope::User, ResourceType::Themes) => self.settings.set_theme_paths(paths),
        }
    }

    fn toggle_package_resource(&mut self, item: &ResourceItem, enabled: bool) {
        let scope = if item.metadata.scope == SourceScope::Project {
            SettingsScope::Project
        } else {
            SettingsScope::User
        };
        let settings = match scope {
            SettingsScope::Project => self.settings.get_project_settings(),
            SettingsScope::User => self.settings.get_global_settings(),
        };
        let mut packages: Vec<Value> = settings
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let Some(index) = packages
            .iter()
            .position(|package| package_source_string(package) == item.metadata.source)
        else {
            return;
        };

        let mut package = match &packages[index] {
            Value::String(source) => {
                let mut map = serde_json::Map::new();
                map.insert("source".to_owned(), Value::String(source.clone()));
                Value::Object(map)
            }
            other => other.clone(),
        };

        let array_key = item.resource_type.key();
        let current: Vec<String> = package
            .get(array_key)
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let pattern = self.get_package_resource_pattern(item);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| pattern_entry_target(entry) != pattern)
            .collect();
        updated.push(format!("{}{pattern}", if enabled { "+" } else { "-" }));
        if let Some(object) = package.as_object_mut() {
            if updated.is_empty() {
                object.remove(array_key);
            } else {
                object.insert(
                    array_key.to_owned(),
                    Value::Array(updated.into_iter().map(Value::String).collect()),
                );
            }
            let has_filters = ResourceType::ALL
                .iter()
                .any(|resource_type| object.get(resource_type.key()).is_some());
            if !has_filters {
                packages[index] = Value::String(package_source_string(&package));
            } else {
                packages[index] = package;
            }
        }

        match scope {
            SettingsScope::Project => self.settings.set_project_packages(&packages),
            SettingsScope::User => self.settings.set_packages(&packages),
        }
    }
}

/// senpi's buildInheritedEnabledMap.
pub fn build_inherited_enabled_map(groups: &[ResourceGroup]) -> BTreeMap<String, bool> {
    let mut result = BTreeMap::new();
    for group in groups {
        for subgroup in &group.subgroups {
            for item in &subgroup.items {
                result.insert(resource_item_key(item), item.enabled);
            }
        }
    }
    result
}

fn resource_item_key(item: &ResourceItem) -> String {
    format!(
        "{}:{}",
        item.resource_type.key(),
        maho_core::paths::canonicalize_path(&item.path)
    )
}

fn subgroup_label_key(group: &ResourceGroup, subgroup: &ResourceSubgroup) -> String {
    format!("{}:{}", group.key, subgroup.resource_type.key())
}

/// senpi's relative(): the path relative to base, or the path itself when it is not under base.
fn relative_path(base: &str, path: &str) -> String {
    match Path::new(path).strip_prefix(base) {
        Ok(rest) => rest.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_owned(),
    }
}

impl ResourceList {
    fn get_item_scope(&self, item: &ResourceItem) -> SettingsScope {
        if item.metadata.scope == SourceScope::Project {
            SettingsScope::Project
        } else {
            SettingsScope::User
        }
    }

    fn get_top_level_base_dir(&self, scope: SettingsScope) -> String {
        match scope {
            SettingsScope::Project => Path::new(&self.cwd).join(&self.config_dir_name).to_string_lossy().into_owned(),
            SettingsScope::User => self.agent_dir.clone(),
        }
    }

    fn get_resource_pattern(&self, item: &ResourceItem) -> String {
        let scope = item.metadata.scope;
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| self.get_top_level_base_dir(if scope == SourceScope::Project { SettingsScope::Project } else { SettingsScope::User }));
        relative_path(&base_dir, &item.path)
    }

    fn get_package_resource_pattern(&self, item: &ResourceItem) -> String {
        let base_dir = item.metadata.base_dir.clone().unwrap_or_else(|| {
            Path::new(&item.path)
                .parent()
                .map_or_else(String::new, |parent| parent.to_string_lossy().into_owned())
        });
        relative_path(&base_dir, &item.path)
    }

    fn get_resource_pattern_for_scope(&self, item: &ResourceItem, scope: SettingsScope) -> String {
        if scope != self.get_item_scope(item) {
            return item.path.clone();
        }
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| self.get_top_level_base_dir(scope));
        relative_path(&base_dir, &item.path)
    }

    fn get_top_level_override_patterns(&self, item: &ResourceItem, scope: SettingsScope) -> BTreeSet<String> {
        let base_dir = self.get_top_level_base_dir(scope);
        let mut patterns: BTreeSet<String> = BTreeSet::new();
        patterns.insert(self.get_resource_pattern_for_scope(item, scope));
        patterns.insert(item.path.clone());
        patterns.insert(relative_path(&base_dir, &item.path));
        if let Some(metadata_base_dir) = &item.metadata.base_dir {
            patterns.insert(relative_path(metadata_base_dir, &item.path));
        }
        patterns
    }

    fn get_inherited_enabled(&self, item: &ResourceItem) -> bool {
        self.inherited_enabled_by_key
            .get(&resource_item_key(item))
            .copied()
            .unwrap_or_else(|| self.get_item_scope(item) == SettingsScope::User || item.enabled)
    }

    fn is_inherited_global_item(&self, item: &ResourceItem) -> bool {
        self.get_item_scope(item) == SettingsScope::User || self.inherited_enabled_by_key.contains_key(&resource_item_key(item))
    }

    fn get_override_state_from_entries(
        &self,
        entries: &[String],
        patterns: &BTreeSet<String>,
        empty_array_is_unload: bool,
    ) -> ProjectOverrideState {
        if entries.is_empty() && empty_array_is_unload {
            return ProjectOverrideState::Unload;
        }
        let mut state = ProjectOverrideState::Inherit;
        for entry in entries {
            if !patterns.contains(pattern_entry_target(entry)) {
                continue;
            }
            state = if entry.starts_with('!') || entry.starts_with('-') {
                ProjectOverrideState::Unload
            } else {
                ProjectOverrideState::Load
            };
        }
        state
    }

    fn find_matching_package_source(&self, item: &ResourceItem, target_scope: SettingsScope) -> Option<Value> {
        let settings = match target_scope {
            SettingsScope::Project => self.settings.get_project_settings(),
            SettingsScope::User => self.settings.get_global_settings(),
        };
        settings.get("packages").and_then(Value::as_array).and_then(|packages| {
            packages.iter().find(|package| {
                self.package_source_string_matches(
                    &item.metadata.source,
                    self.get_item_scope(item),
                    &package_source_string(package),
                    target_scope,
                )
            }).cloned()
        })
    }

    fn package_source_string_matches(
        &self,
        left_source: &str,
        left_scope: SettingsScope,
        right_source: &str,
        right_scope: SettingsScope,
    ) -> bool {
        if left_source == right_source {
            return true;
        }
        if !maho_core::paths::is_local_path(left_source) || !maho_core::paths::is_local_path(right_source) {
            return false;
        }
        let options = maho_core::paths::PathInputOptions { trim: true, ..Default::default() };
        let left = maho_core::paths::resolve_path(left_source, &self.get_top_level_base_dir(left_scope), &options);
        let right = maho_core::paths::resolve_path(right_source, &self.get_top_level_base_dir(right_scope), &options);
        left == right
    }

    fn create_package_override_source(&self, item: &ResourceItem) -> Value {
        let source = item.metadata.source.clone();
        if !maho_core::paths::is_local_path(&source) {
            return serde_json::json!({ "source": source, "autoload": false });
        }
        let options = maho_core::paths::PathInputOptions { trim: true, ..Default::default() };
        let source_path =
            maho_core::paths::resolve_path(&source, &self.get_top_level_base_dir(self.get_item_scope(item)), &options);
        let project_base = self.get_top_level_base_dir(SettingsScope::Project);
        let relative = relative_path(&project_base, &source_path);
        let source = if relative.is_empty() { ".".to_owned() } else { relative };
        serde_json::json!({ "source": source, "autoload": false })
    }

    fn get_project_override_state(&self, item: &ResourceItem) -> ProjectOverrideState {
        if self.write_scope != ConfigWriteScope::Project {
            return ProjectOverrideState::Inherit;
        }
        if item.metadata.origin == maho_core::source_info::SourceOrigin::TopLevel {
            let entries = settings_array(&self.settings.get_project_settings(), item.resource_type.key());
            let patterns = self.get_top_level_override_patterns(item, SettingsScope::Project);
            return self.get_override_state_from_entries(&entries, &patterns, false);
        }
        let Some(package) = self.find_matching_package_source(item, SettingsScope::Project) else {
            return ProjectOverrideState::Inherit;
        };
        if !package.is_object() {
            return ProjectOverrideState::Inherit;
        }
        let Some(entries) = package.get(item.resource_type.key()) else {
            return ProjectOverrideState::Inherit;
        };
        let entries: Vec<String> = entries
            .as_array()
            .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        let patterns: BTreeSet<String> = BTreeSet::from([self.get_package_resource_pattern(item)]);
        self.get_override_state_from_entries(&entries, &patterns, package.get("autoload").and_then(Value::as_bool) != Some(false))
    }

    fn get_next_override_state(&self, item: &ResourceItem) -> ProjectOverrideState {
        let state = self.get_project_override_state(item);
        let inherited_enabled = self.get_inherited_enabled(item);
        match state {
            ProjectOverrideState::Inherit => {
                if inherited_enabled {
                    ProjectOverrideState::Unload
                } else {
                    ProjectOverrideState::Load
                }
            }
            ProjectOverrideState::Unload => {
                if inherited_enabled {
                    ProjectOverrideState::Load
                } else {
                    ProjectOverrideState::Inherit
                }
            }
            ProjectOverrideState::Load => {
                if inherited_enabled {
                    ProjectOverrideState::Inherit
                } else {
                    ProjectOverrideState::Unload
                }
            }
        }
    }

    fn set_project_resource_override(&mut self, item: &ResourceItem, state: ProjectOverrideState) -> bool {
        if item.metadata.origin == maho_core::source_info::SourceOrigin::TopLevel {
            self.set_project_top_level_override(item, state)
        } else {
            self.set_project_package_override(item, state)
        }
    }

    fn set_project_top_level_override(&mut self, item: &ResourceItem, state: ProjectOverrideState) -> bool {
        let current = settings_array(&self.settings.get_project_settings(), item.resource_type.key());
        let pattern = if self.is_inherited_global_item(item) {
            item.path.clone()
        } else {
            self.get_resource_pattern_for_scope(item, SettingsScope::Project)
        };
        let patterns = self.get_top_level_override_patterns(item, SettingsScope::Project);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| {
                let target = pattern_entry_target(entry);
                if (entry.starts_with('!') || entry.starts_with('+') || entry.starts_with('-'))
                    && patterns.contains(target)
                {
                    return false;
                }
                !(state == ProjectOverrideState::Inherit
                    && self.is_inherited_global_item(item)
                    && target == pattern)
            })
            .collect();
        if state != ProjectOverrideState::Inherit {
            if self.is_inherited_global_item(item) && !updated.iter().any(|entry| entry == &pattern) {
                updated.push(pattern.clone());
            }
            updated.push(format!(
                "{}{pattern}",
                if state == ProjectOverrideState::Load { "+" } else { "-" }
            ));
        }
        self.write_top_level_paths(SettingsScope::Project, item.resource_type, &updated);
        true
    }

    fn set_project_package_override(&mut self, item: &ResourceItem, state: ProjectOverrideState) -> bool {
        let settings = self.settings.get_project_settings();
        let mut packages: Vec<Value> = settings
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut index = packages.iter().position(|package| {
            self.package_source_string_matches(
                &item.metadata.source,
                self.get_item_scope(item),
                &package_source_string(package),
                SettingsScope::Project,
            )
        });
        if index.is_none() {
            if state == ProjectOverrideState::Inherit {
                return false;
            }
            packages.push(self.create_package_override_source(item));
            index = Some(packages.len() - 1);
        }
        let Some(index) = index else {
            return false;
        };
        let mut package = match &packages[index] {
            Value::String(source) => serde_json::json!({ "source": source }),
            other => other.clone(),
        };
        let pattern = self.get_package_resource_pattern(item);
        let updated: Vec<String> = package
            .get(item.resource_type.key())
            .and_then(Value::as_array)
            .map(|values| -> Vec<String> {
                values.iter().filter_map(Value::as_str).map(str::to_owned).collect()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| pattern_entry_target(entry) != pattern)
            .collect();
        let mut updated = updated;
        if state != ProjectOverrideState::Inherit {
            updated.push(format!(
                "{}{pattern}",
                if state == ProjectOverrideState::Load { "+" } else { "-" }
            ));
        }
        if let Some(object) = package.as_object_mut() {
            if updated.is_empty() {
                object.remove(item.resource_type.key());
            } else {
                object.insert(
                    item.resource_type.key().to_owned(),
                    Value::Array(updated.into_iter().map(Value::String).collect()),
                );
            }
            let has_filters = ResourceType::ALL
                .iter()
                .any(|resource_type| object.get(resource_type.key()).is_some());
            if !has_filters {
                let autoload_false = object.get("autoload").and_then(Value::as_bool) == Some(false);
                if autoload_false {
                    packages.remove(index);
                } else {
                    packages[index] = Value::String(package_source_string(&package));
                }
            } else {
                packages[index] = package;
            }
        }
        self.settings.set_project_packages(&packages);
        true
    }
}

impl Component for ResourceList {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        if self.filtered_items.is_empty() {
            lines.push(self.theme.fg(ThemeColor::Muted, "  No resources found"));
            return lines;
        }

        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible / 2)
            .min(self.filtered_items.len().saturating_sub(self.max_visible));
        let end_index = (start_index + self.max_visible).min(self.filtered_items.len());

        for index in start_index..end_index {
            let entry = self.filtered_items[index].clone();
            let is_selected = index == self.selected_index;
            match entry {
                FlatEntry::Group(group) => {
                    let inherited =
                        self.write_scope == ConfigWriteScope::Project && group.scope == SourceScope::User;
                    let label = self.theme.bold(&format!(
                        "{}{}",
                        group.label,
                        if inherited { " · inherited global" } else { "" }
                    ));
                    let group_line = self
                        .theme
                        .fg(if inherited { ThemeColor::Dim } else { ThemeColor::Accent }, &label);
                    lines.push(truncate_to_width(&format!("  {group_line}"), width, "", false));
                }
                FlatEntry::Subgroup { subgroup, group } => {
                    let color = if self.write_scope == ConfigWriteScope::Project
                        && group.scope == SourceScope::User
                    {
                        ThemeColor::Dim
                    } else {
                        ThemeColor::Muted
                    };
                    let subgroup_line = self.theme.fg(color, &subgroup.label);
                    lines.push(truncate_to_width(&format!("    {subgroup_line}"), width, "", false));
                }
                FlatEntry::Item(item) => {
                    let cursor = if is_selected { "> " } else { "  " };
                    let dimmed = self.is_dimmed_item(&item);
                    let name_text = if is_selected && !dimmed {
                        self.theme.bold(&item.display_name)
                    } else {
                        item.display_name.clone()
                    };
                    let name = if dimmed { self.theme.fg(ThemeColor::Dim, &name_text) } else { name_text };
                    lines.push(truncate_to_width(
                        &format!(
                            "{cursor}    {} {name}{}",
                            self.render_checkbox(&item),
                            self.get_item_suffix(&item)
                        ),
                        width,
                        "...",
                        false,
                    ));
                }
            }
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            let item_count = self
                .filtered_items
                .iter()
                .filter(|entry| matches!(entry, FlatEntry::Item(_)))
                .count();
            let current_item_index = self.filtered_items[..self.selected_index]
                .iter()
                .filter(|entry| matches!(entry, FlatEntry::Item(_)))
                .count()
                + 1;
            lines.push(self.theme.fg(ThemeColor::Dim, &format!("  ({current_item_index}/{item_count})")));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();

        if kb.matches(data, "tui.select.up") {
            self.selected_index = self.find_next_item(self.selected_index, -1);
            return;
        }
        if kb.matches(data, "tui.select.down") {
            self.selected_index = self.find_next_item(self.selected_index, 1);
            return;
        }
        if kb.matches(data, "tui.select.pageUp") {
            let mut target = self.selected_index.saturating_sub(self.max_visible);
            while target < self.filtered_items.len() && !matches!(self.filtered_items[target], FlatEntry::Item(_)) {
                target += 1;
            }
            if target < self.filtered_items.len() {
                self.selected_index = target;
            }
            return;
        }
        if kb.matches(data, "tui.select.pageDown") {
            let mut target = (self.selected_index + self.max_visible)
                .min(self.filtered_items.len().saturating_sub(1));
            while target > 0 && !matches!(self.filtered_items[target], FlatEntry::Item(_)) {
                target -= 1;
            }
            if matches!(self.filtered_items.get(target), Some(FlatEntry::Item(_))) {
                self.selected_index = target;
            }
            return;
        }
        if kb.matches(data, "tui.select.cancel") {
            if let Some(callback) = &mut self.on_cancel {
                callback();
            }
            return;
        }
        if matches_key(data, "ctrl+c") {
            if let Some(callback) = &mut self.on_exit {
                callback();
            }
            return;
        }
        if kb.matches(data, "tui.input.tab") {
            if let Some(callback) = &mut self.on_switch_mode {
                callback();
            }
            return;
        }
        if data == " " || kb.matches(data, "tui.select.confirm") {
            let entry = self.filtered_items.get(self.selected_index).cloned();
            if let Some(FlatEntry::Item(item)) = entry
                && (self.write_scope == ConfigWriteScope::Project
                    || self.get_item_scope(&item) == SettingsScope::User)
                && let Some(new_enabled) = self.toggle_resource(&item)
            {
                self.update_item(&item, new_enabled);
                if let Some(callback) = &mut self.on_toggle {
                    callback(&item, new_enabled);
                }
            }
            return;
        }

        self.search_input.handle_input(data);
        let query = self.search_input.get_value().to_owned();
        self.filter_items(&query);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.search_input.set_focused(focused);
    }

    fn invalidate(&mut self) {
        self.search_input.invalidate();
    }
}

impl Focusable for ResourceList {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.search_input.set_focused(value);
    }
}

/// senpi's ConfigSelectorComponent.
pub struct ConfigSelectorComponent {
    theme: Theme,
    header: ConfigSelectorHeader,
    resource_list: ResourceList,
    write_scope: ConfigWriteScope,
    focused: bool,
}

impl ConfigSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        resolved_paths: BTreeMap<ConfigWriteScope, ResolvedPaths>,
        settings: Box<dyn ConfigSettingsHost>,
        cwd: &str,
        agent_dir: &str,
        config_dir_name: &str,
        on_close: Box<dyn FnMut()>,
        on_exit: Box<dyn FnMut()>,
        request_render: Box<dyn FnMut()>,
        terminal_height: Option<usize>,
        write_scope: ConfigWriteScope,
        project_mode_available: bool,
    ) -> Self {
        let mut groups_by_scope: BTreeMap<ConfigWriteScope, Vec<ResourceGroup>> = BTreeMap::new();
        for (scope, resolved) in &resolved_paths {
            groups_by_scope.insert(
                *scope,
                build_groups(resolved, agent_dir, &home_dir(), config_dir_name),
            );
        }
        let header = ConfigSelectorHeader::new(theme, write_scope, project_mode_available, config_dir_name);
        let mut resource_list = ResourceList::new(
            theme,
            groups_by_scope,
            settings,
            cwd,
            agent_dir,
            config_dir_name,
            terminal_height,
            write_scope,
        );
        resource_list.on_cancel = Some(on_close);
        resource_list.on_exit = Some(on_exit);
        let mut request_render = request_render;
        resource_list.on_toggle = Some(Box::new(move |_item, _enabled| request_render()));
        Self {
            theme: theme.clone(),
            header,
            resource_list,
            write_scope,
            focused: false,
        }
    }

    pub fn set_switch_mode(&mut self, on_switch_mode: Box<dyn FnMut()>) {
        self.resource_list.on_switch_mode = Some(on_switch_mode);
    }

    pub fn resource_list(&self) -> &ResourceList {
        &self.resource_list
    }

    pub fn resource_list_mut(&mut self) -> &mut ResourceList {
        &mut self.resource_list
    }

    pub fn switch_write_scope(&mut self) {
        self.write_scope = match self.write_scope {
            ConfigWriteScope::Global => ConfigWriteScope::Project,
            ConfigWriteScope::Project => ConfigWriteScope::Global,
        };
        self.header.set_write_scope(self.write_scope);
        self.resource_list.set_write_scope(self.write_scope);
    }
}

impl Component for ConfigSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![
            String::new(),
            dynamic_border(&self.theme, ThemeColor::Border, width),
            String::new(),
        ];
        lines.extend(self.header.render(width));
        lines.push(String::new());
        lines.extend(self.resource_list.render(width));
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.resource_list.handle_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.resource_list.set_focused(focused);
    }

    fn invalidate(&mut self) {
        self.resource_list.invalidate();
    }
}

impl Focusable for ConfigSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.resource_list.set_focused(value);
    }
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}
