//! Port of components/tree-selector.ts.
//!
//! senpi's SessionTreeNode comes from core/session-manager.ts (plan todo 21); maho-core already
//! ports it as an entry serde_json::Value tree, which is what this component walks. Entries are
//! read through the same JSON keys senpi's typed union uses.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use maho_core::session_manager::SessionTreeNode;
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::keybindings::{KeybindingsManager, get_keybindings};
use maho_tui::tui::{Component, Focusable};
use maho_tui::utils::{slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi};
use serde_json::Value;

use super::keybinding_hints::{format_key_text, key_hint};
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeBg, ThemeColor};

/// Callback shapes senpi stores as plain function fields.
pub type EntrySelectHandler = Box<dyn FnMut(&str)>;
pub type VoidHandler = Box<dyn FnMut()>;
pub type OptionalTextHandler = Box<dyn FnMut(Option<&str>)>;
pub type LabelChangeHandler = Box<dyn FnMut(&str, Option<&str>)>;

/// One entry of the recompute stack used by [`TreeList::recalculate_visual_structure`].
type RecalcStackItem = (String, usize, bool, bool, bool, Vec<GutterInfo>, bool);

/// Gutter info: position (displayIndent where connector was) and whether to show the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GutterInfo {
    pub position: usize,
    pub show: bool,
}

/// Flattened tree node for navigation.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatNode {
    pub node: SessionTreeNode,
    /// Indentation level (each level = 3 chars).
    pub indent: usize,
    /// Whether to show a connector; true if the parent has multiple children.
    pub show_connector: bool,
    /// If show_connector, true = last sibling, false = not last.
    pub is_last: bool,
    /// Gutter info for each ancestor branch point.
    pub gutters: Vec<GutterInfo>,
    /// True if this node is a root under a virtual branching root (multiple roots).
    pub is_virtual_root_child: bool,
}

#[derive(Debug, Clone)]
struct HorizontalViewportRow {
    gutter: String,
    body: String,
    anchor_col: usize,
    body_width: usize,
    is_selected: bool,
}

const TREE_GUTTER_WIDTH: usize = 2;
const MIN_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 4;
const MAX_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 20;
const MIN_ANCHOR_CONTEXT_WIDTH: usize = 2;
const MAX_ANCHOR_CONTEXT_WIDTH: usize = 12;

/// Render tree rows into a horizontally clipped viewport.
fn render_horizontal_viewport(rows: &[HorizontalViewportRow], width: usize) -> Vec<String> {
    let viewport_width = width.saturating_sub(TREE_GUTTER_WIDTH);
    let max_body_width = rows.iter().map(|row| row.body_width).max().unwrap_or(0);
    let max_horizontal_scroll = max_body_width.saturating_sub(viewport_width);
    let selected_row = rows.iter().find(|row| row.is_selected);

    let mut horizontal_scroll = 0;
    if let Some(selected_row) = selected_row
        && max_horizontal_scroll > 0
    {
        let min_visible_anchor_content_width = MAX_VISIBLE_ANCHOR_CONTENT_WIDTH
            .min(MIN_VISIBLE_ANCHOR_CONTENT_WIDTH.max(viewport_width / 3));
        if selected_row.anchor_col > viewport_width.saturating_sub(min_visible_anchor_content_width) {
            let anchor_context_width =
                MAX_ANCHOR_CONTEXT_WIDTH.min(MIN_ANCHOR_CONTEXT_WIDTH.max(viewport_width / 4));
            horizontal_scroll =
                max_horizontal_scroll.min(selected_row.anchor_col.saturating_sub(anchor_context_width));
        }
    }

    rows.iter()
        .map(|row| {
            let line = if horizontal_scroll > 0 {
                format!(
                    "{}{}\u{1b}[0m",
                    row.gutter,
                    slice_by_column(&row.body, horizontal_scroll, viewport_width, true)
                )
            } else {
                format!("{}{}", row.gutter, row.body)
            };
            truncate_to_width(&line, width, "", false)
        })
        .collect()
}

/// Filter mode for tree display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

const FILTER_MODES: [FilterMode; 5] = [
    FilterMode::Default,
    FilterMode::NoTools,
    FilterMode::UserOnly,
    FilterMode::LabeledOnly,
    FilterMode::All,
];

/// Tool call info for lookup.
#[derive(Debug, Clone, PartialEq)]
struct ToolCallInfo {
    name: String,
    arguments: Value,
}

fn entry_str<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn entry_id(entry: &Value) -> String {
    entry_str(entry, "id").unwrap_or_default().to_owned()
}

fn entry_parent_id(entry: &Value) -> Option<String> {
    entry.get("parentId").and_then(Value::as_str).map(str::to_owned)
}

fn entry_type(entry: &Value) -> &str {
    entry_str(entry, "type").unwrap_or_default()
}

fn message_role(entry: &Value) -> Option<&str> {
    entry.get("message").and_then(|message| entry_str(message, "role"))
}

fn entry_id_of(flat: &FlatNode) -> String {
    entry_id(&flat.node.entry)
}

fn arg_str_owned(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn has_text_content(content: Option<&Value>) -> bool {
    match content {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(blocks)) => blocks.iter().any(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()),
            Some("providerNative") => true,
            _ => false,
        }),
        _ => false,
    }
}

/// Today's date parts as (year, month, day) in UTC.
fn today_parts() -> (String, String, String) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let days = (seconds / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (format!("{year:04}"), format!("{month:02}"), format!("{day:02}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchDirection {
    Up,
    Down,
}

struct StackItem {
    node: SessionTreeNode,
    indent: usize,
    just_branched: bool,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<GutterInfo>,
    is_virtual_root_child: bool,
}

/// Tree list component with selection and ASCII art visualization.
pub struct TreeList {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    flat_nodes: Vec<FlatNode>,
    filtered_nodes: Vec<FlatNode>,
    selected_index: usize,
    current_leaf_id: Option<String>,
    max_visible_lines: usize,
    filter_mode: FilterMode,
    search_query: String,
    tool_call_map: BTreeMap<String, ToolCallInfo>,
    multiple_roots: bool,
    show_label_timestamps: bool,
    active_path_ids: BTreeSet<String>,
    visible_parent_map: BTreeMap<String, Option<String>>,
    visible_children_map: BTreeMap<Option<String>, Vec<String>>,
    last_selected_id: Option<String>,
    folded_nodes: BTreeSet<String>,
    home: Option<String>,
    pub on_select: Option<EntrySelectHandler>,
    pub on_cancel: Option<VoidHandler>,
    pub on_copy: Option<OptionalTextHandler>,
    pub on_edit_message: Option<EntrySelectHandler>,
    pub on_label_edit: Option<LabelChangeHandler>,
}

impl TreeList {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<&str>,
        max_visible_lines: usize,
        initial_selected_id: Option<&str>,
        initial_filter_mode: Option<FilterMode>,
        home: Option<String>,
    ) -> Self {
        let mut list = Self {
            theme: theme.clone(),
            keybindings,
            flat_nodes: Vec::new(),
            filtered_nodes: Vec::new(),
            selected_index: 0,
            current_leaf_id: current_leaf_id.map(str::to_owned),
            max_visible_lines,
            filter_mode: initial_filter_mode.unwrap_or(FilterMode::Default),
            search_query: String::new(),
            tool_call_map: BTreeMap::new(),
            multiple_roots: tree.len() > 1,
            show_label_timestamps: false,
            active_path_ids: BTreeSet::new(),
            visible_parent_map: BTreeMap::new(),
            visible_children_map: BTreeMap::new(),
            last_selected_id: None,
            folded_nodes: BTreeSet::new(),
            home,
            on_select: None,
            on_cancel: None,
            on_copy: None,
            on_edit_message: None,
            on_label_edit: None,
        };
        list.flat_nodes = list.flatten_tree(&tree);
        list.build_active_path();
        list.apply_filter();
        let target = initial_selected_id.map(str::to_owned).or_else(|| list.current_leaf_id.clone());
        list.selected_index = list.find_nearest_visible_index(target.as_deref());
        list.last_selected_id = list
            .filtered_nodes
            .get(list.selected_index)
            .map(entry_id_of);
        list
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn filter_mode(&self) -> FilterMode {
        self.filter_mode
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn max_visible_lines(&self) -> usize {
        self.max_visible_lines
    }

    pub fn filtered_entry_ids(&self) -> Vec<String> {
        self.filtered_nodes.iter().map(entry_id_of).collect()
    }

    pub fn get_selected_node(&self) -> Option<&SessionTreeNode> {
        self.filtered_nodes.get(self.selected_index).map(|flat| &flat.node)
    }

    pub fn show_label_timestamps(&self) -> bool {
        self.show_label_timestamps
    }

    pub fn folded_ids(&self) -> Vec<String> {
        self.folded_nodes.iter().cloned().collect()
    }

    /// senpi's findNearestVisibleIndex.
    fn find_nearest_visible_index(&self, target: Option<&str>) -> usize {
        if self.filtered_nodes.is_empty() {
            return 0;
        }
        let mut entry_map: BTreeMap<String, Option<String>> = BTreeMap::new();
        for flat in &self.flat_nodes {
            entry_map.insert(entry_id_of(flat), entry_parent_id(&flat.node.entry));
        }
        let visible: BTreeMap<String, usize> = self
            .filtered_nodes
            .iter()
            .enumerate()
            .map(|(index, flat)| (entry_id_of(flat), index))
            .collect();

        let mut current = target.map(str::to_owned);
        while let Some(id) = current {
            if let Some(index) = visible.get(&id) {
                return *index;
            }
            match entry_map.get(&id) {
                Some(parent) => current = parent.clone(),
                None => break,
            }
        }
        self.filtered_nodes.len() - 1
    }

    /// Build the set of entry IDs on the path from root to the current leaf.
    fn build_active_path(&mut self) {
        self.active_path_ids.clear();
        let Some(leaf) = self.current_leaf_id.clone() else {
            return;
        };
        let mut entry_map: BTreeMap<String, Option<String>> = BTreeMap::new();
        for flat in &self.flat_nodes {
            entry_map.insert(entry_id_of(flat), entry_parent_id(&flat.node.entry));
        }
        let mut current = Some(leaf);
        while let Some(id) = current {
            self.active_path_ids.insert(id.clone());
            match entry_map.get(&id) {
                Some(parent) => current = parent.clone(),
                None => break,
            }
        }
    }

    fn flatten_tree(&mut self, roots: &[SessionTreeNode]) -> Vec<FlatNode> {
        self.tool_call_map.clear();
        let mut result = Vec::new();

        // Which subtrees contain the active leaf (used to sort the current branch first).
        let mut contains_active: BTreeMap<String, bool> = BTreeMap::new();
        {
            let mut all_nodes: Vec<&SessionTreeNode> = Vec::new();
            let mut pre_order: Vec<&SessionTreeNode> = roots.iter().collect();
            while let Some(node) = pre_order.pop() {
                all_nodes.push(node);
                for child in node.children.iter().rev() {
                    pre_order.push(child);
                }
            }
            for node in all_nodes.iter().rev() {
                let leaf_id = self.current_leaf_id.as_deref();
                let mut has = leaf_id.is_some_and(|leaf| entry_id(&node.entry) == leaf);
                for child in &node.children {
                    if contains_active.get(&entry_id(&child.entry)).copied().unwrap_or(false) {
                        has = true;
                    }
                }
                contains_active.insert(entry_id(&node.entry), has);
            }
        }

        let multiple_roots = roots.len() > 1;
        let mut ordered_roots: Vec<&SessionTreeNode> = roots.iter().collect();
        ordered_roots
            .sort_by_key(|node| !contains_active.get(&entry_id(&node.entry)).copied().unwrap_or(false));
        let mut stack: Vec<StackItem> = Vec::new();
        for (index, root) in ordered_roots.iter().enumerate().rev() {
            let is_last = index == ordered_roots.len() - 1;
            stack.push(StackItem {
                node: (*root).clone(),
                indent: if multiple_roots { 1 } else { 0 },
                just_branched: multiple_roots,
                show_connector: multiple_roots,
                is_last,
                gutters: Vec::new(),
                is_virtual_root_child: multiple_roots,
            });
        }

        while let Some(item) = stack.pop() {
            let entry = &item.node.entry;
            if entry_type(entry) == "message"
                && message_role(entry) == Some("assistant")
                && let Some(blocks) = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
            {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("toolCall")
                        && let Some(id) = block.get("id").and_then(Value::as_str)
                    {
                        self.tool_call_map.insert(
                            id.to_owned(),
                            ToolCallInfo {
                                name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                                arguments: block.get("arguments").cloned().unwrap_or(Value::Null),
                            },
                        );
                    }
                }
            }

            result.push(FlatNode {
                node: item.node.clone(),
                indent: item.indent,
                show_connector: item.show_connector,
                is_last: item.is_last,
                gutters: item.gutters.clone(),
                is_virtual_root_child: item.is_virtual_root_child,
            });

            let children = &item.node.children;
            let multiple_children = children.len() > 1;
            let mut prioritized: Vec<&SessionTreeNode> = Vec::new();
            let mut rest: Vec<&SessionTreeNode> = Vec::new();
            for child in children {
                if contains_active.get(&entry_id(&child.entry)).copied().unwrap_or(false) {
                    prioritized.push(child);
                } else {
                    rest.push(child);
                }
            }
            let ordered_children: Vec<&SessionTreeNode> = prioritized.into_iter().chain(rest).collect();

            let child_indent = if multiple_children || (item.just_branched && item.indent > 0) {
                item.indent + 1
            } else {
                item.indent
            };

            let connector_displayed = item.show_connector && !item.is_virtual_root_child;
            let current_display_indent = if self.multiple_roots {
                item.indent.saturating_sub(1)
            } else {
                item.indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut gutters = item.gutters.clone();
                gutters.push(GutterInfo { position: connector_position, show: !item.is_last });
                gutters
            } else {
                item.gutters.clone()
            };

            for (index, child) in ordered_children.iter().enumerate().rev() {
                let child_is_last = index == ordered_children.len() - 1;
                stack.push(StackItem {
                    node: (*child).clone(),
                    indent: child_indent,
                    just_branched: multiple_children,
                    show_connector: multiple_children,
                    is_last: child_is_last,
                    gutters: child_gutters.clone(),
                    is_virtual_root_child: false,
                });
            }
        }

        result
    }

    fn apply_filter(&mut self) {
        if !self.filtered_nodes.is_empty() {
            self.last_selected_id = self
                .filtered_nodes
                .get(self.selected_index)
                .map(entry_id_of)
                .or_else(|| self.last_selected_id.clone());
        }

        let search_tokens: Vec<String> = self
            .search_query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        let current_leaf = self.current_leaf_id.clone();
        let filter_mode = self.filter_mode;
        let searchable = self.build_searchable_text();
        let mut filtered: Vec<FlatNode> = Vec::new();
        for flat in &self.flat_nodes {
            let entry = &flat.node.entry;
            let is_current_leaf = current_leaf.as_deref() == Some(entry_id(entry).as_str());

            if entry_type(entry) == "message" && message_role(entry) == Some("assistant") && !is_current_leaf {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                let stop_reason = message.get("stopReason").and_then(Value::as_str);
                let has_text = has_text_content(message.get("content"));
                let is_error_or_aborted = stop_reason.is_some_and(|reason| reason != "stop" && reason != "toolUse");
                if !has_text && !is_error_or_aborted {
                    continue;
                }
            }

            let is_settings_entry = matches!(
                entry_type(entry),
                "label"
                    | "custom"
                    | "model_change"
                    | "model_change_rejected"
                    | "thinking_level_change"
                    | "session_info"
            );
            let passes_filter = match filter_mode {
                FilterMode::UserOnly => entry_type(entry) == "message" && message_role(entry) == Some("user"),
                FilterMode::NoTools => {
                    !(is_settings_entry
                        || entry_type(entry) == "message" && message_role(entry) == Some("toolResult"))
                }
                FilterMode::LabeledOnly => flat.node.label.is_some(),
                FilterMode::All => true,
                FilterMode::Default => !is_settings_entry,
            };
            if !passes_filter {
                continue;
            }

            if !search_tokens.is_empty() {
                let node_text = searchable
                    .get(&entry_id_of(flat))
                    .cloned()
                    .unwrap_or_default()
                    .to_lowercase();
                if !search_tokens.iter().all(|token| node_text.contains(token)) {
                    continue;
                }
            }
            filtered.push(flat.clone());
        }
        self.filtered_nodes = filtered;

        // Filter out descendants of folded nodes.
        if !self.folded_nodes.is_empty() {
            let mut skip_set: BTreeSet<String> = BTreeSet::new();
            for flat in &self.flat_nodes {
                let id = entry_id_of(flat);
                if let Some(parent_id) = entry_parent_id(&flat.node.entry)
                    && (self.folded_nodes.contains(&parent_id) || skip_set.contains(&parent_id))
                {
                    skip_set.insert(id);
                }
            }
            self.filtered_nodes.retain(|flat| !skip_set.contains(&entry_id_of(flat)));
        }

        self.recalculate_visual_structure();

        if let Some(last) = self.last_selected_id.clone() {
            self.selected_index = self.find_nearest_visible_index(Some(&last));
        } else if self.selected_index >= self.filtered_nodes.len() {
            self.selected_index = self.filtered_nodes.len().saturating_sub(1);
        }

        if !self.filtered_nodes.is_empty() {
            self.last_selected_id = self.filtered_nodes.get(self.selected_index).map(entry_id_of);
        }
    }

    /// senpi's getSearchableText, precomputed for every flat node.
    fn build_searchable_text(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        for flat in &self.flat_nodes {
            map.insert(entry_id_of(flat), self.get_searchable_text(&flat.node));
        }
        map
    }

    /// Recompute indentation/connectors for the filtered view.
    fn recalculate_visual_structure(&mut self) {
        if self.filtered_nodes.is_empty() {
            return;
        }
        let visible_ids: BTreeSet<String> = self.filtered_nodes.iter().map(entry_id_of).collect();
        let entry_map: BTreeMap<String, Option<String>> = self
            .flat_nodes
            .iter()
            .map(|flat| (entry_id_of(flat), entry_parent_id(&flat.node.entry)))
            .collect();

        let find_visible_ancestor = |node_id: &str| -> Option<String> {
            let mut current = entry_map.get(node_id).cloned().flatten();
            while let Some(id) = current {
                if visible_ids.contains(&id) {
                    return Some(id);
                }
                current = entry_map.get(&id).cloned().flatten();
            }
            None
        };

        let mut visible_parent: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut visible_children: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
        visible_children.insert(None, Vec::new());
        for flat in &self.filtered_nodes {
            let node_id = entry_id_of(flat);
            let ancestor_id = find_visible_ancestor(&node_id);
            visible_parent.insert(node_id.clone(), ancestor_id.clone());
            visible_children.entry(ancestor_id).or_default().push(node_id);
        }

        let visible_root_ids = visible_children.get(&None).cloned().unwrap_or_default();
        let multiple_roots = visible_root_ids.len() > 1;

        let mut filtered_node_map: BTreeMap<String, FlatNode> = BTreeMap::new();
        for flat in &self.filtered_nodes {
            filtered_node_map.insert(entry_id_of(flat), flat.clone());
        }

        let mut stack: Vec<RecalcStackItem> = Vec::new();
        for (index, root_id) in visible_root_ids.iter().enumerate().rev() {
            let is_last = index == visible_root_ids.len() - 1;
            stack.push((
                root_id.clone(),
                if multiple_roots { 1 } else { 0 },
                multiple_roots,
                multiple_roots,
                is_last,
                Vec::new(),
                multiple_roots,
            ));
        }

        let mut updates: BTreeMap<String, FlatNode> = BTreeMap::new();
        while let Some((node_id, indent, just_branched, show_connector, is_last, gutters, is_virtual_root_child)) =
            stack.pop()
        {
            let Some(mut flat) = filtered_node_map.get(&node_id).cloned() else {
                continue;
            };
            flat.indent = indent;
            flat.show_connector = show_connector;
            flat.is_last = is_last;
            flat.gutters = gutters.clone();
            flat.is_virtual_root_child = is_virtual_root_child;
            updates.insert(node_id.clone(), flat);

            let children = visible_children.get(&Some(node_id.clone())).cloned().unwrap_or_default();
            let multiple_children = children.len() > 1;
            let child_indent = if multiple_children || (just_branched && indent > 0) {
                indent + 1
            } else {
                indent
            };
            let connector_displayed = show_connector && !is_virtual_root_child;
            let current_display_indent = if multiple_roots { indent.saturating_sub(1) } else { indent };
            let connector_position = current_display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut gutters = gutters.clone();
                gutters.push(GutterInfo { position: connector_position, show: !is_last });
                gutters
            } else {
                gutters.clone()
            };

            for (index, child) in children.iter().enumerate().rev() {
                let child_is_last = index == children.len() - 1;
                stack.push((
                    child.clone(),
                    child_indent,
                    multiple_children,
                    multiple_children,
                    child_is_last,
                    child_gutters.clone(),
                    false,
                ));
            }
        }

        for flat in &mut self.filtered_nodes {
            if let Some(updated) = updates.get(&entry_id_of(flat)) {
                *flat = updated.clone();
            }
        }
        self.multiple_roots = multiple_roots;
        self.visible_parent_map = visible_parent;
        self.visible_children_map = visible_children;
    }

    /// senpi's getSearchableText.
    fn get_searchable_text(&self, node: &SessionTreeNode) -> String {
        let entry = &node.entry;
        let mut parts: Vec<String> = Vec::new();
        if let Some(label) = &node.label {
            parts.push(label.clone());
        }
        match entry_type(entry) {
            "message" => {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                parts.push(message_role(entry).unwrap_or_default().to_owned());
                if let Some(content) = message.get("content") {
                    parts.push(self.extract_content(Some(content)));
                }
                if message_role(entry) == Some("bashExecution")
                    && let Some(command) = message.get("command").and_then(Value::as_str)
                {
                    parts.push(command.to_owned());
                }
            }
            "custom_message" => {
                parts.push(entry_str(entry, "customType").unwrap_or_default().to_owned());
                match entry.get("content") {
                    Some(Value::String(text)) => parts.push(text.clone()),
                    other => parts.push(self.extract_content(other)),
                }
            }
            "compaction" => parts.push("compaction".to_owned()),
            "branch_summary" => {
                parts.push("branch summary".to_owned());
                parts.push(entry_str(entry, "summary").unwrap_or_default().to_owned());
            }
            "session_info" => {
                parts.push("title".to_owned());
                if let Some(name) = entry_str(entry, "name") {
                    parts.push(name.to_owned());
                }
            }
            "model_change" => {
                parts.push("model".to_owned());
                parts.push(entry_str(entry, "modelId").unwrap_or_default().to_owned());
            }
            "model_change_rejected" => {
                parts.push("model rejected".to_owned());
                parts.push(entry_str(entry, "modelId").unwrap_or_default().to_owned());
                parts.push(entry_str(entry, "reason").unwrap_or_default().to_owned());
            }
            "thinking_level_change" => {
                parts.push("thinking".to_owned());
                parts.push(entry_str(entry, "thinkingLevel").unwrap_or_default().to_owned());
            }
            "custom" => {
                parts.push("custom".to_owned());
                parts.push(entry_str(entry, "customType").unwrap_or_default().to_owned());
            }
            "label" => {
                parts.push("label".to_owned());
                parts.push(entry_str(entry, "label").unwrap_or_default().to_owned());
            }
            _ => {}
        }
        parts.join(" ")
    }

    fn status_labels(&self) -> String {
        let mut labels = String::new();
        match self.filter_mode {
            FilterMode::NoTools => labels.push_str(" [no-tools]"),
            FilterMode::UserOnly => labels.push_str(" [user]"),
            FilterMode::LabeledOnly => labels.push_str(" [labeled]"),
            FilterMode::All => labels.push_str(" [all]"),
            FilterMode::Default => {}
        }
        if self.show_label_timestamps {
            labels.push_str(" [+label time]");
        }
        labels
    }

    /// senpi's formatLabelTimestamp, computed from the entry's ISO-8601 timestamp.
    fn format_label_timestamp(timestamp: &str) -> String {
        let (date, time) = match timestamp.split_once('T') {
            Some((date, rest)) => (date, rest.get(..5).unwrap_or_default()),
            None => (timestamp, ""),
        };
        let mut parts = date.split('-');
        let year = parts.next().unwrap_or_default();
        let month = parts.next().unwrap_or_default();
        let day = parts.next().unwrap_or_default();
        let (today_year, today_month, today_day) = today_parts();
        if year == today_year && month == today_month && day == today_day {
            return time.to_owned();
        }
        let month_number = month.trim_start_matches('0');
        let day_number = day.trim_start_matches('0');
        if year == today_year {
            return format!("{month_number}/{day_number} {time}");
        }
        format!("{}/{month_number}/{day_number} {time}", year.get(2..).unwrap_or(year))
    }

    fn format_tool_call(&self, name: &str, args: &Value) -> String {
        let shorten = |path: String| match self.home.as_deref().filter(|home| !home.is_empty()) {
            Some(home) if path.starts_with(home) => format!("~{}", &path[home.len()..]),
            _ => path,
        };
        let path_arg = |args: &Value| {
            arg_str_owned(args, "path")
                .or_else(|| arg_str_owned(args, "file_path"))
                .unwrap_or_default()
        };
        match name {
            "read" => {
                let mut display = shorten(path_arg(args));
                let offset = args.get("offset").and_then(Value::as_i64);
                let limit = args.get("limit").and_then(Value::as_i64);
                if offset.is_some() || limit.is_some() {
                    let start = offset.unwrap_or(1);
                    let end = limit.map(|limit| start + limit - 1);
                    display.push(':');
                    display.push_str(&start.to_string());
                    if let Some(end) = end {
                        display.push_str(&format!("-{end}"));
                    }
                }
                format!("[read: {display}]")
            }
            "write" => format!("[write: {}]", shorten(path_arg(args))),
            "edit" => format!("[edit: {}]", shorten(path_arg(args))),
            "bash" => {
                let raw = args.get("command").and_then(Value::as_str).unwrap_or_default();
                let cmd: String = raw.chars().map(|ch| if ch == '\n' || ch == '\t' { ' ' } else { ch }).collect();
                let cmd = cmd.trim();
                let truncated: String = cmd.chars().take(50).collect();
                let ellipsis = if cmd.chars().count() > 50 { "..." } else { "" };
                format!("[bash: {truncated}{ellipsis}]")
            }
            "grep" => format!(
                "[grep: /{}/ in {}]",
                args.get("pattern").and_then(Value::as_str).unwrap_or_default(),
                shorten(arg_str_owned(args, "path").unwrap_or_else(|| ".".to_owned()))
            ),
            "find" => format!(
                "[find: {} in {}]",
                args.get("pattern").and_then(Value::as_str).unwrap_or_default(),
                shorten(arg_str_owned(args, "path").unwrap_or_else(|| ".".to_owned()))
            ),
            "ls" => format!("[ls: {}]", shorten(arg_str_owned(args, "path").unwrap_or_else(|| ".".to_owned()))),
            _ => {
                let args_str = serde_json::to_string(args).unwrap_or_default();
                let truncated: String = args_str.chars().take(40).collect();
                let ellipsis = if args_str.chars().count() > 40 { "..." } else { "" };
                format!("[{name}: {truncated}{ellipsis}]")
            }
        }
    }

    fn extract_content(&self, content: Option<&Value>) -> String {
        self.extract_full_content(content).chars().take(200).collect()
    }

    fn extract_full_content(&self, content: Option<&Value>) -> String {
        match content {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(blocks)) => {
                let mut result = String::new();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            result.push_str(block.get("text").and_then(Value::as_str).unwrap_or_default());
                        }
                        Some("providerNative") => result.push_str(&format!(
                            "[providerNative:{}]",
                            block.get("subtype").and_then(Value::as_str).unwrap_or("unknown")
                        )),
                        _ => {}
                    }
                }
                result
            }
            _ => String::new(),
        }
    }

    fn normalize(text: &str) -> String {
        text.chars()
            .map(|ch| if ch == '\n' || ch == '\t' { ' ' } else { ch })
            .collect::<String>()
            .trim()
            .to_owned()
    }

    fn get_entry_display_text(&self, node: &SessionTreeNode, is_selected: bool) -> String {
        let entry = &node.entry;
        let result = match entry_type(entry) {
            "message" => {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                match message_role(entry).unwrap_or_default() {
                    "user" => format!(
                        "{}{}",
                        self.theme.fg(ThemeColor::Accent, "user: "),
                        Self::normalize(&self.extract_content(message.get("content")))
                    ),
                    "assistant" => {
                        let text_content = Self::normalize(&self.extract_content(message.get("content")));
                        if !text_content.is_empty() {
                            format!("{}{text_content}", self.theme.fg(ThemeColor::Success, "assistant: "))
                        } else if message.get("stopReason").and_then(Value::as_str) == Some("aborted") {
                            format!(
                                "{}{}",
                                self.theme.fg(ThemeColor::Success, "assistant: "),
                                self.theme.fg(ThemeColor::Muted, "(aborted)")
                            )
                        } else if let Some(error) = message.get("errorMessage").and_then(Value::as_str) {
                            let error: String = Self::normalize(error).chars().take(80).collect();
                            format!(
                                "{}{}",
                                self.theme.fg(ThemeColor::Success, "assistant: "),
                                self.theme.fg(ThemeColor::Error, &error)
                            )
                        } else {
                            format!(
                                "{}{}",
                                self.theme.fg(ThemeColor::Success, "assistant: "),
                                self.theme.fg(ThemeColor::Muted, "(no content)")
                            )
                        }
                    }
                    "toolResult" => {
                        let tool_call_id = message.get("toolCallId").and_then(Value::as_str);
                        match tool_call_id.and_then(|id| self.tool_call_map.get(id)) {
                            Some(call) => self
                                .theme
                                .fg(ThemeColor::Muted, &self.format_tool_call(&call.name, &call.arguments)),
                            None => self.theme.fg(
                                ThemeColor::Muted,
                                &format!(
                                    "[{}]",
                                    message.get("toolName").and_then(Value::as_str).unwrap_or("tool")
                                ),
                            ),
                        }
                    }
                    "bashExecution" => self.theme.fg(
                        ThemeColor::Dim,
                        &format!(
                            "[bash]: {}",
                            Self::normalize(message.get("command").and_then(Value::as_str).unwrap_or_default())
                        ),
                    ),
                    role => self.theme.fg(ThemeColor::Dim, &format!("[{role}]")),
                }
            }
            "custom_message" => {
                let content = match entry.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    other => self.extract_content(other),
                };
                format!(
                    "{}{}",
                    self.theme.fg(
                        ThemeColor::CustomMessageLabel,
                        &format!("[{}]: ", entry_str(entry, "customType").unwrap_or_default())
                    ),
                    Self::normalize(&content)
                )
            }
            "compaction" => {
                let tokens = entry.get("tokensBefore").and_then(Value::as_f64).unwrap_or(0.0).round() / 1000.0;
                self.theme
                    .fg(ThemeColor::BorderAccent, &format!("[compaction: {}k tokens]", tokens as i64))
            }
            "branch_summary" => format!(
                "{}{}",
                self.theme.fg(ThemeColor::Warning, "[branch summary]: "),
                Self::normalize(entry_str(entry, "summary").unwrap_or_default())
            ),
            "model_change" => self.theme.fg(
                ThemeColor::Dim,
                &format!("[model: {}]", entry_str(entry, "modelId").unwrap_or_default()),
            ),
            "model_change_rejected" => self.theme.fg(
                ThemeColor::Warning,
                &format!(
                    "[model rejected: {} ({})]",
                    entry_str(entry, "modelId").unwrap_or_default(),
                    entry_str(entry, "reason").unwrap_or_default()
                ),
            ),
            "thinking_level_change" => self.theme.fg(
                ThemeColor::Dim,
                &format!("[thinking: {}]", entry_str(entry, "thinkingLevel").unwrap_or_default()),
            ),
            "custom" => self.theme.fg(
                ThemeColor::Dim,
                &format!("[custom: {}]", entry_str(entry, "customType").unwrap_or_default()),
            ),
            "label" => self.theme.fg(
                ThemeColor::Dim,
                &format!("[label: {}]", entry_str(entry, "label").unwrap_or("(cleared)")),
            ),
            "session_info" => match entry_str(entry, "name") {
                Some(name) => format!(
                    "{}{}{}",
                    self.theme.fg(ThemeColor::Dim, "[title: "),
                    self.theme.fg(ThemeColor::Dim, name),
                    self.theme.fg(ThemeColor::Dim, "]")
                ),
                None => format!(
                    "{}{}{}",
                    self.theme.fg(ThemeColor::Dim, "[title: "),
                    self.theme.italic(&self.theme.fg(ThemeColor::Dim, "empty")),
                    self.theme.fg(ThemeColor::Dim, "]")
                ),
            },
            _ => String::new(),
        };

        if is_selected { self.theme.bold(&result) } else { result }
    }
}

impl TreeList {
    fn is_foldable(&self, entry_id: &str) -> bool {
        let Some(children) = self.visible_children_map.get(&Some(entry_id.to_owned())) else {
            return false;
        };
        if children.is_empty() {
            return false;
        }
        match self.visible_parent_map.get(entry_id) {
            None | Some(None) => true,
            Some(Some(parent_id)) => self
                .visible_children_map
                .get(&Some(parent_id.clone()))
                .is_some_and(|siblings| siblings.len() > 1),
        }
    }

    /// senpi's findBranchSegmentStart.
    fn find_branch_segment_start(&self, direction: BranchDirection) -> usize {
        let Some(selected_id) = self.filtered_nodes.get(self.selected_index).map(entry_id_of) else {
            return self.selected_index;
        };
        let index_by_entry_id: BTreeMap<String, usize> = self
            .filtered_nodes
            .iter()
            .enumerate()
            .map(|(index, flat)| (entry_id_of(flat), index))
            .collect();

        let mut current_id = selected_id;
        if direction == BranchDirection::Down {
            loop {
                let children = self
                    .visible_children_map
                    .get(&Some(current_id.clone()))
                    .cloned()
                    .unwrap_or_default();
                if children.is_empty() {
                    return index_by_entry_id.get(&current_id).copied().unwrap_or(self.selected_index);
                }
                if children.len() > 1 {
                    return index_by_entry_id
                        .get(&children[0])
                        .copied()
                        .unwrap_or(self.selected_index);
                }
                current_id = children[0].clone();
            }
        }

        loop {
            let Some(parent_id) = self.visible_parent_map.get(&current_id).cloned().flatten() else {
                return index_by_entry_id.get(&current_id).copied().unwrap_or(self.selected_index);
            };
            let children = self
                .visible_children_map
                .get(&Some(parent_id.clone()))
                .cloned()
                .unwrap_or_default();
            if children.len() > 1
                && let Some(segment_start) = index_by_entry_id.get(&current_id).copied()
                && segment_start < self.selected_index
            {
                return segment_start;
            }
            current_id = parent_id;
        }
    }

    pub fn update_node_label(&mut self, entry_id: &str, label: Option<&str>, label_timestamp: Option<&str>) {
        for flat in &mut self.flat_nodes {
            if entry_id_of(flat) == entry_id {
                flat.node.label = label.map(str::to_owned);
                flat.node.label_timestamp = label.map(|_| {
                    label_timestamp
                        .map(str::to_owned)
                        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned())
                });
                break;
            }
        }
    }

    pub fn copy_selected(&mut self) {
        let text = self.get_selected_node().and_then(|node| self.get_entry_copy_text(node));
        if let Some(callback) = &mut self.on_copy {
            callback(text.as_deref());
        }
    }

    /// Assistant responses open the edit flow; user-authored messages reuse the select flow.
    pub fn edit_selected(&mut self) {
        let Some(entry) = self.get_selected_node().map(|node| node.entry.clone()) else {
            return;
        };
        if entry_type(&entry) == "message" && message_role(&entry) == Some("assistant") {
            let id = entry_id(&entry);
            if let Some(callback) = &mut self.on_edit_message {
                callback(&id);
            }
        } else if (entry_type(&entry) == "message" && message_role(&entry) == Some("user"))
            || entry_type(&entry) == "custom_message"
        {
            let id = entry_id(&entry);
            if let Some(callback) = &mut self.on_select {
                callback(&id);
            }
        }
    }

    fn get_entry_copy_text(&self, node: &SessionTreeNode) -> Option<String> {
        let entry = &node.entry;
        let text = match entry_type(entry) {
            "message" => {
                let message = entry.get("message").cloned().unwrap_or(Value::Null);
                if message_role(entry) == Some("bashExecution") {
                    message.get("command").and_then(Value::as_str).map(str::to_owned)
                } else if message.get("content").is_some() {
                    let text = self.extract_full_content(message.get("content"));
                    if text.is_empty() && message_role(entry) == Some("assistant") {
                        message.get("errorMessage").and_then(Value::as_str).map(str::to_owned)
                    } else {
                        Some(text)
                    }
                } else {
                    None
                }
            }
            "custom_message" => Some(self.extract_full_content(entry.get("content"))),
            "compaction" | "branch_summary" => entry_str(entry, "summary").map(str::to_owned),
            _ => None,
        };
        text.filter(|text| !text.trim().is_empty())
    }
}

impl Component for TreeList {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        if self.filtered_nodes.is_empty() {
            lines.push(truncate_to_width(
                &self.theme.fg(ThemeColor::Muted, "  No entries found"),
                width,
                "",
                false,
            ));
            lines.push(truncate_to_width(
                &self
                    .theme
                    .fg(ThemeColor::Muted, &format!("  (0/0){}", self.status_labels())),
                width,
                "",
                false,
            ));
            return lines;
        }

        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible_lines / 2)
            .min(self.filtered_nodes.len().saturating_sub(self.max_visible_lines));
        let end_index = (start_index + self.max_visible_lines).min(self.filtered_nodes.len());

        let mut rendered_rows: Vec<HorizontalViewportRow> = Vec::new();
        for index in start_index..end_index {
            let flat_node = self.filtered_nodes[index].clone();
            let is_selected = index == self.selected_index;

            let cursor = if is_selected {
                self.theme.fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_owned()
            };

            let display_indent = if self.multiple_roots {
                flat_node.indent.saturating_sub(1)
            } else {
                flat_node.indent
            };

            let connector = if flat_node.show_connector && !flat_node.is_virtual_root_child {
                if flat_node.is_last { "└─ " } else { "├─ " }
            } else {
                ""
            };
            let connector_position = if connector.is_empty() {
                None
            } else {
                Some(display_indent.saturating_sub(1))
            };

            let total_chars = display_indent * 3;
            let entry_id_value = entry_id_of(&flat_node);
            let is_folded = self.folded_nodes.contains(&entry_id_value);
            let mut prefix_chars: Vec<char> = Vec::with_capacity(total_chars);
            for i in 0..total_chars {
                let level = i / 3;
                let pos_in_level = i % 3;
                if let Some(gutter) = flat_node.gutters.iter().find(|gutter| gutter.position == level) {
                    prefix_chars.push(if pos_in_level == 0 && gutter.show { '│' } else { ' ' });
                } else if connector_position == Some(level) {
                    if pos_in_level == 0 {
                        prefix_chars.push(if flat_node.is_last { '└' } else { '├' });
                    } else if pos_in_level == 1 {
                        let foldable = self.is_foldable(&entry_id_value);
                        prefix_chars.push(if is_folded {
                            '⊞'
                        } else if foldable {
                            '⊟'
                        } else {
                            '─'
                        });
                    } else {
                        prefix_chars.push(' ');
                    }
                } else {
                    prefix_chars.push(' ');
                }
            }
            let prefix: String = prefix_chars.into_iter().collect();

            let shows_fold_in_connector = flat_node.show_connector && !flat_node.is_virtual_root_child;
            let fold_marker = if is_folded && !shows_fold_in_connector {
                self.theme.fg(ThemeColor::Accent, "⊞ ")
            } else {
                String::new()
            };

            let path_marker = if self.active_path_ids.contains(&entry_id_value) {
                self.theme.fg(ThemeColor::Accent, "• ")
            } else {
                String::new()
            };

            let label = match &flat_node.node.label {
                Some(label) => self.theme.fg(ThemeColor::Warning, &format!("[{label}] ")),
                None => String::new(),
            };
            let label_timestamp = if self.show_label_timestamps {
                match (&flat_node.node.label, &flat_node.node.label_timestamp) {
                    (Some(_), Some(timestamp)) => {
                        self.theme
                            .fg(ThemeColor::Muted, &format!("{} ", Self::format_label_timestamp(timestamp)))
                    }
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            let content = self.get_entry_display_text(&flat_node.node, is_selected);
            let prefix_part = format!("{}{fold_marker}{path_marker}", self.theme.fg(ThemeColor::Dim, &prefix));
            let anchor_col = visible_width(&prefix_part);
            let mut gutter = cursor;
            let mut body = format!("{prefix_part}{label}{label_timestamp}{content}");
            if is_selected {
                gutter = self.theme.bg(ThemeBg::SelectedBg, &gutter);
                body = self.theme.bg(ThemeBg::SelectedBg, &body);
            }
            rendered_rows.push(HorizontalViewportRow {
                gutter,
                anchor_col,
                body_width: visible_width(&body),
                body,
                is_selected,
            });
        }

        lines.extend(render_horizontal_viewport(&rendered_rows, width));
        lines.push(truncate_to_width(
            &self.theme.fg(
                ThemeColor::Muted,
                &format!(
                    "  ({}/{}){}",
                    self.selected_index + 1,
                    self.filtered_nodes.len(),
                    self.status_labels()
                ),
            ),
            width,
            "",
            false,
        ));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = Arc::clone(&self.keybindings);
        if kb.matches(data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_nodes.len().saturating_sub(1)
            } else {
                self.selected_index - 1
            };
        } else if kb.matches(data, "tui.select.down") {
            self.selected_index = if self.selected_index == self.filtered_nodes.len().saturating_sub(1) {
                0
            } else {
                self.selected_index + 1
            };
        } else if kb.matches(data, "app.tree.foldOrUp") {
            let current_id = self.filtered_nodes.get(self.selected_index).map(entry_id_of);
            match current_id {
                Some(id) if self.is_foldable(&id) && !self.folded_nodes.contains(&id) => {
                    self.folded_nodes.insert(id);
                    self.apply_filter();
                }
                _ => self.selected_index = self.find_branch_segment_start(BranchDirection::Up),
            }
        } else if kb.matches(data, "app.tree.unfoldOrDown") {
            let current_id = self.filtered_nodes.get(self.selected_index).map(entry_id_of);
            match current_id {
                Some(id) if self.folded_nodes.contains(&id) => {
                    self.folded_nodes.remove(&id);
                    self.apply_filter();
                }
                _ => self.selected_index = self.find_branch_segment_start(BranchDirection::Down),
            }
        } else if kb.matches(data, "tui.editor.cursorLeft") || kb.matches(data, "tui.select.pageUp") {
            self.selected_index = self.selected_index.saturating_sub(self.max_visible_lines);
        } else if kb.matches(data, "tui.editor.cursorRight") || kb.matches(data, "tui.select.pageDown") {
            self.selected_index = (self.selected_index + self.max_visible_lines)
                .min(self.filtered_nodes.len().saturating_sub(1));
        } else if kb.matches(data, "tui.select.confirm") {
            if let Some(selected) = self.filtered_nodes.get(self.selected_index) {
                let id = entry_id_of(selected);
                if let Some(callback) = &mut self.on_select {
                    callback(&id);
                }
            }
        } else if kb.matches(data, "app.message.copy") {
            self.copy_selected();
        } else if kb.matches(data, "app.tree.editMessage") {
            self.edit_selected();
        } else if kb.matches(data, "tui.select.cancel") {
            if self.search_query.is_empty() {
                if let Some(callback) = &mut self.on_cancel {
                    callback();
                }
            } else {
                self.search_query.clear();
                self.folded_nodes.clear();
                self.apply_filter();
            }
        } else if kb.matches(data, "app.tree.filter.default") {
            self.filter_mode = FilterMode::Default;
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "app.tree.filter.noTools") {
            self.filter_mode = if self.filter_mode == FilterMode::NoTools {
                FilterMode::Default
            } else {
                FilterMode::NoTools
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "app.tree.filter.userOnly") {
            self.filter_mode = if self.filter_mode == FilterMode::UserOnly {
                FilterMode::Default
            } else {
                FilterMode::UserOnly
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "app.tree.filter.labeledOnly") {
            self.filter_mode = if self.filter_mode == FilterMode::LabeledOnly {
                FilterMode::Default
            } else {
                FilterMode::LabeledOnly
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "app.tree.filter.all") {
            self.filter_mode = if self.filter_mode == FilterMode::All {
                FilterMode::Default
            } else {
                FilterMode::All
            };
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "app.tree.filter.cycleBackward") {
            let current = FILTER_MODES.iter().position(|mode| *mode == self.filter_mode).unwrap_or(0);
            self.filter_mode = FILTER_MODES[(current + FILTER_MODES.len() - 1) % FILTER_MODES.len()];
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "app.tree.filter.cycleForward") {
            let current = FILTER_MODES.iter().position(|mode| *mode == self.filter_mode).unwrap_or(0);
            self.filter_mode = FILTER_MODES[(current + 1) % FILTER_MODES.len()];
            self.folded_nodes.clear();
            self.apply_filter();
        } else if kb.matches(data, "tui.editor.deleteCharBackward") {
            if !self.search_query.is_empty() {
                self.search_query.pop();
                self.folded_nodes.clear();
                self.apply_filter();
            }
        } else if kb.matches(data, "app.tree.editLabel") {
            if let Some(selected) = self.filtered_nodes.get(self.selected_index) {
                let id = entry_id_of(selected);
                let label = selected.node.label.clone();
                if let Some(callback) = &mut self.on_label_edit {
                    callback(&id, label.as_deref());
                }
            }
        } else if kb.matches(data, "app.tree.toggleLabelTimestamp") {
            self.show_label_timestamps = !self.show_label_timestamps;
        } else {
            let has_control_chars = data.chars().any(|ch| {
                let code = ch as u32;
                code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
            });
            if !has_control_chars && !data.is_empty() {
                self.search_query.push_str(data);
                self.folded_nodes.clear();
                self.apply_filter();
            }
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(false)
    }

    fn invalidate(&mut self) {}
}

impl Focusable for TreeList {
    fn focused(&self) -> bool {
        false
    }

    fn set_focused(&mut self, _value: bool) {}
}

/// Component that displays the current search query.
pub struct SearchLine {
    query: String,
    theme: Theme,
}

impl SearchLine {
    pub fn new(theme: &Theme, query: String) -> Self {
        Self { query, theme: theme.clone() }
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
    }
}

impl Component for SearchLine {
    fn render(&mut self, width: usize) -> Vec<String> {
        let label = self.theme.fg(ThemeColor::Muted, "Type to search:");
        let line = if self.query.is_empty() {
            format!("  {label}")
        } else {
            format!("  {label} {}", self.theme.fg(ThemeColor::Accent, &self.query))
        };
        vec![truncate_to_width(&line, width, "", false)]
    }

    fn handle_input(&mut self, _data: &str) {}

    fn invalidate(&mut self) {}
}

/// Component that renders tree help as semantic rows with chunk-aware wrapping.
pub struct TreeHelp {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
}

impl TreeHelp {
    pub fn new(theme: &Theme, keybindings: Arc<KeybindingsManager>) -> Self {
        Self { theme: theme.clone(), keybindings }
    }
}

impl Component for TreeHelp {
    fn render(&mut self, width: usize) -> Vec<String> {
        let items: Vec<String> = TREE_HELP_ITEMS
            .iter()
            .map(|(keys, label, label_first)| {
                let text = format_help_keys(&self.keybindings, keys);
                if text.is_empty() {
                    (*label).to_owned()
                } else if *label_first {
                    format!("{label} {text}")
                } else {
                    format!("{text} {label}")
                }
            })
            .collect();

        let available_width = width.max(1);
        let indent = "  ";
        let separator = " · ";
        let mut lines: Vec<String> = Vec::new();
        let mut current_line = String::new();

        for item in &items {
            let candidate = if current_line.is_empty() {
                if visible_width(&format!("{indent}{item}")) <= available_width {
                    format!("{indent}{item}")
                } else {
                    item.clone()
                }
            } else {
                format!("{current_line}{separator}{item}")
            };
            if current_line.is_empty() || visible_width(&candidate) <= available_width {
                current_line = candidate;
                continue;
            }
            lines.extend(wrap_text_with_ansi(current_line.trim_end(), available_width));
            current_line = if visible_width(&format!("{indent}{item}")) <= available_width {
                format!("{indent}{item}")
            } else {
                item.clone()
            };
        }
        if !current_line.is_empty() {
            lines.extend(wrap_text_with_ansi(current_line.trim_end(), available_width));
        }
        lines
            .into_iter()
            .map(|line| self.theme.fg(ThemeColor::Muted, &line))
            .collect()
    }

    fn invalidate(&mut self) {}
}

const TREE_HELP_ITEMS: &[(&[&str], &str, bool)] = &[
    (&["tui.select.up", "tui.select.down"], "move", false),
    (&["tui.editor.cursorLeft", "tui.editor.cursorRight"], "page", false),
    (&["app.tree.foldOrUp", "app.tree.unfoldOrDown"], "branch", false),
    (&["app.message.copy"], "copy", false),
    (&["app.tree.editMessage"], "edit", false),
    (&["app.tree.editLabel"], "label", false),
    (&["app.tree.toggleLabelTimestamp"], "label time", false),
    (
        &[
            "app.tree.filter.default",
            "app.tree.filter.noTools",
            "app.tree.filter.userOnly",
            "app.tree.filter.labeledOnly",
            "app.tree.filter.all",
        ],
        "filters",
        true,
    ),
    (
        &["app.tree.filter.cycleForward", "app.tree.filter.cycleBackward"],
        "cycle",
        true,
    ),
];

fn format_help_keys(keybindings: &KeybindingsManager, keybinding_ids: &[&str]) -> String {
    let keys: Vec<String> = keybinding_ids
        .iter()
        .filter_map(|id| keybindings.get_keys(id).first().cloned())
        .collect();
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&compact_raw_keys(&keys), false)
        .replace("pageUp", "pgup")
        .replace("pageDown", "pgdn")
        .replace("up", "↑")
        .replace("down", "↓")
        .replace("left", "←")
        .replace("right", "→")
}

fn compact_raw_keys(keys: &[String]) -> String {
    if keys.len() == 1 {
        return keys[0].clone();
    }
    let parts: Vec<(&str, &str)> = keys
        .iter()
        .map(|key| match key.rfind('+') {
            Some(index) => (&key[..=index], &key[index + 1..]),
            None => ("", key.as_str()),
        })
        .collect();
    let prefix = parts[0].0;
    if !prefix.is_empty() && parts.iter().all(|(candidate, _)| *candidate == prefix) {
        format!(
            "{prefix}{}",
            parts.iter().map(|(_, suffix)| *suffix).collect::<Vec<_>>().join("/")
        )
    } else {
        keys.join("/")
    }
}

/// Label input component shown when editing a label.
pub struct LabelInput {
    theme: Theme,
    input: Input,
    entry_id: String,
    focused: bool,
    pub on_submit: Option<LabelChangeHandler>,
    pub on_cancel: Option<VoidHandler>,
}

impl LabelInput {
    pub fn new(theme: &Theme, entry_id: &str, current_label: Option<&str>) -> Self {
        let mut input = Input::new(InputOptions::default());
        if let Some(label) = current_label {
            input.set_value(label);
        }
        Self {
            theme: theme.clone(),
            input,
            entry_id: entry_id.to_owned(),
            focused: false,
            on_submit: None,
            on_cancel: None,
        }
    }

    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }

    pub fn input(&self) -> &Input {
        &self.input
    }

    /// senpi's Enter branch: the trimmed value, empty meaning remove.
    pub fn submitted_label(&self) -> Option<String> {
        let value = self.input.get_value().trim().to_owned();
        if value.is_empty() { None } else { Some(value) }
    }
}

impl Component for LabelInput {
    fn render(&mut self, width: usize) -> Vec<String> {
        let indent = "  ";
        let available_width = width.saturating_sub(indent.len());
        let mut lines = vec![truncate_to_width(
            &format!("{indent}{}", self.theme.fg(ThemeColor::Muted, "Label (empty to remove):")),
            width,
            "",
            false,
        )];
        lines.extend(
            self.input
                .render(available_width)
                .into_iter()
                .map(|line| truncate_to_width(&format!("{indent}{line}"), width, "", false)),
        );
        lines.push(truncate_to_width(
            &format!(
                "{indent}{}  {}",
                key_hint(&self.theme, "tui.select.confirm", "save"),
                key_hint(&self.theme, "tui.select.cancel", "cancel")
            ),
            width,
            "",
            false,
        ));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.confirm") {
            let label = self.submitted_label();
            if let Some(callback) = &mut self.on_submit {
                callback(&self.entry_id, label.as_deref());
            }
        } else if kb.matches(data, "tui.select.cancel") {
            if let Some(callback) = &mut self.on_cancel {
                callback();
            }
        } else {
            self.input.handle_input(data);
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }
}

impl Focusable for LabelInput {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.set_focused(value);
    }
}

/// Component that renders a session tree selector for navigation.
pub struct TreeSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    tree_list: Rc<RefCell<TreeList>>,
    search_line: SearchLine,
    label_input: Option<LabelInput>,
    on_label_change: Option<LabelChangeHandler>,
    on_cancel: VoidHandler,
    /// Set by the tree list's cancel callback and drained in handle_input.
    cancel_pending: Rc<std::cell::Cell<bool>>,
    focused: bool,
    pub on_copy: Option<OptionalTextHandler>,
    pub on_edit_message: Option<EntrySelectHandler>,
}

impl TreeSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<&str>,
        terminal_height: usize,
        on_select: EntrySelectHandler,
        on_cancel: VoidHandler,
        on_label_change: Option<LabelChangeHandler>,
        initial_selected_id: Option<&str>,
        initial_filter_mode: Option<FilterMode>,
        home: Option<String>,
    ) -> Self {
        let max_visible_lines = (terminal_height / 2).max(5);
        let mut tree_list = TreeList::new(
            theme,
            Arc::clone(&keybindings),
            tree,
            current_leaf_id,
            max_visible_lines,
            initial_selected_id,
            initial_filter_mode,
            home,
        );
        let mut on_select = on_select;
        tree_list.on_select = Some(Box::new(move |entry_id| on_select(entry_id)));
        let cancel_pending = Rc::new(std::cell::Cell::new(false));
        let cancel_signal = Rc::clone(&cancel_pending);
        tree_list.on_cancel = Some(Box::new(move || cancel_signal.set(true)));
        let search_line = SearchLine::new(theme, tree_list.search_query().to_owned());
        Self {
            theme: theme.clone(),
            keybindings,
            tree_list: Rc::new(RefCell::new(tree_list)),
            search_line,
            label_input: None,
            on_label_change,
            on_cancel,
            cancel_pending,
            focused: false,
            on_copy: None,
            on_edit_message: None,
        }
    }

    pub fn tree_list(&self) -> &Rc<RefCell<TreeList>> {
        &self.tree_list
    }

    pub fn showing_label_input(&self) -> bool {
        self.label_input.is_some()
    }

    pub fn label_input(&self) -> Option<&LabelInput> {
        self.label_input.as_ref()
    }

    /// senpi's showLabelInput.
    pub fn show_label_input(&mut self, entry_id: &str, current_label: Option<&str>) {
        let mut label_input = LabelInput::new(&self.theme, entry_id, current_label);
        label_input.set_focused(self.focused);
        self.label_input = Some(label_input);
    }

    pub fn hide_label_input(&mut self) {
        self.label_input = None;
    }

    /// Apply a submitted label to the tree and notify the host (senpi's submit branch).
    pub fn apply_label_input(&mut self, entry_id: &str, label: Option<&str>) {
        self.tree_list.borrow_mut().update_node_label(entry_id, label, None);
        if let Some(callback) = &mut self.on_label_change {
            callback(entry_id, label);
        }
        self.hide_label_input();
    }
}

impl Component for TreeSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = vec![
            String::new(),
            dynamic_border(&self.theme, ThemeColor::Border, width),
        ];
        lines.extend(Text::with_padding(self.theme.bold("  Session Tree"), 1, 0).render(width));
        lines.extend(TreeHelp::new(&self.theme, Arc::clone(&self.keybindings)).render(width));
        self.search_line
            .set_query(self.tree_list.borrow().search_query().to_owned());
        lines.extend(self.search_line.render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines.push(String::new());
        match &mut self.label_input {
            Some(label_input) => lines.extend(label_input.render(width)),
            None => lines.extend(self.tree_list.borrow_mut().render(width)),
        }
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if self.label_input.is_some() {
            let kb = Arc::clone(&self.keybindings);
            if kb.matches(data, "tui.select.confirm") {
                let (entry_id, label) = {
                    let label_input = self.label_input.as_ref().expect("checked above");
                    (label_input.entry_id().to_owned(), label_input.submitted_label())
                };
                self.apply_label_input(&entry_id, label.as_deref());
            } else if kb.matches(data, "tui.select.cancel") {
                self.hide_label_input();
            } else if let Some(label_input) = &mut self.label_input {
                label_input.handle_input(data);
            }
            return;
        }
        self.tree_list.borrow_mut().handle_input(data);
        if self.cancel_pending.replace(false) {
            (self.on_cancel)();
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(label_input) = &mut self.label_input {
            label_input.set_focused(focused);
        }
    }

    fn invalidate(&mut self) {
        self.tree_list.borrow_mut().invalidate();
    }
}

impl Focusable for TreeSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        if let Some(label_input) = &mut self.label_input {
            label_input.set_focused(value);
        }
    }
}
