//! Port of components/session-selector.ts.
//!
//! senpi's SessionInfo/SessionTreeNode come from core/session-manager.ts (plan todo 21); the
//! search module carries the same field subset, and this component builds its own display tree from
//! parentSessionPath. Session loading and deletion are host callbacks, so the todo-35 mode owns
//! the filesystem and trash integration.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use maho_core::paths::canonicalize_path;
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::keybindings::KeybindingsManager;
use maho_tui::tui::{Component, Focusable};
use maho_tui::utils::{truncate_to_width, visible_width};

use super::keybinding_hints::{key_hint, key_text};
use super::session_selector_search::{
    NameFilter, SessionInfo, SortMode, filter_and_sort_sessions, has_session_name,
};
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeBg, ThemeColor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScope {
    Current,
    All,
}

/// senpi's SessionListProgress.
pub type SessionListProgress = Box<dyn FnMut(usize, usize)>;
/// senpi's SessionsLoader.
pub type SessionsLoader = Box<dyn FnMut(Option<SessionListProgress>) -> Vec<SessionInfo>>;

pub type SessionPathHandler = Box<dyn FnMut(&str)>;
pub type VoidHandler = Box<dyn FnMut()>;
pub type BoolHandler = Box<dyn FnMut(bool)>;
pub type OptionalPathHandler = Box<dyn FnMut(Option<&str>)>;

/// senpi's shortenPath.
pub fn shorten_path(path: &str, home: Option<&str>) -> String {
    match home.filter(|home| !home.is_empty()) {
        Some(home) if path.starts_with(home) => format!("~{}", &path[home.len()..]),
        _ => path.to_owned(),
    }
}

/// senpi's formatSessionDate: a compact relative age.
pub fn format_session_date(modified_ms: i64, now_ms: i64) -> String {
    let diff_ms = (now_ms - modified_ms).max(0);
    let diff_mins = diff_ms / 60_000;
    let diff_hours = diff_ms / 3_600_000;
    let diff_days = diff_ms / 86_400_000;
    if diff_mins < 1 {
        "now".to_owned()
    } else if diff_mins < 60 {
        format!("{diff_mins}m")
    } else if diff_hours < 24 {
        format!("{diff_hours}h")
    } else if diff_days < 7 {
        format!("{diff_days}d")
    } else if diff_days < 30 {
        format!("{}w", diff_days / 7)
    } else if diff_days < 365 {
        format!("{}mo", diff_days / 30)
    } else {
        format!("{}y", diff_days / 365)
    }
}

/// Process time in epoch milliseconds (senpi reads new Date()).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelectorStatusMessage {
    pub is_error: bool,
    pub message: String,
}

/// senpi's SessionSelectorHeader.
pub struct SessionSelectorHeader {
    theme: Theme,
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    loading: bool,
    load_progress: Option<(usize, usize)>,
    show_path: bool,
    confirming_delete_path: Option<String>,
    status_message: Option<SessionSelectorStatusMessage>,
    show_rename_hint: bool,
}

impl SessionSelectorHeader {
    pub fn new(theme: &Theme, scope: SessionScope, sort_mode: SortMode, name_filter: NameFilter) -> Self {
        Self {
            theme: theme.clone(),
            scope,
            sort_mode,
            name_filter,
            loading: false,
            load_progress: None,
            show_path: false,
            confirming_delete_path: None,
            status_message: None,
            show_rename_hint: false,
        }
    }

    pub fn set_scope(&mut self, scope: SessionScope) {
        self.scope = scope;
    }

    pub fn set_sort_mode(&mut self, sort_mode: SortMode) {
        self.sort_mode = sort_mode;
    }

    pub fn set_name_filter(&mut self, name_filter: NameFilter) {
        self.name_filter = name_filter;
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
        // Progress is scoped to the current load; clear whenever the loading state is set.
        self.load_progress = None;
    }

    pub fn set_progress(&mut self, loaded: usize, total: usize) {
        self.load_progress = Some((loaded, total));
    }

    pub fn set_show_path(&mut self, show_path: bool) {
        self.show_path = show_path;
    }

    pub fn set_show_rename_hint(&mut self, show: bool) {
        self.show_rename_hint = show;
    }

    pub fn set_confirming_delete_path(&mut self, path: Option<String>) {
        self.confirming_delete_path = path;
    }

    pub fn set_status_message(&mut self, message: Option<SessionSelectorStatusMessage>) {
        self.status_message = message;
    }
}

impl Component for SessionSelectorHeader {
    fn render(&mut self, width: usize) -> Vec<String> {
        let title = match self.scope {
            SessionScope::Current => "Resume Session (Current Folder)",
            SessionScope::All => "Resume Session (All)",
        };
        let left_text = self.theme.bold(title);

        let sort_label = match self.sort_mode {
            SortMode::Threaded => "Threaded",
            SortMode::Recent => "Recent",
            SortMode::Relevance => "Fuzzy",
        };
        let sort_text = format!(
            "{}{}",
            self.theme.fg(ThemeColor::Muted, "Sort: "),
            self.theme.fg(ThemeColor::Accent, sort_label)
        );
        let name_label = match self.name_filter {
            NameFilter::All => "All",
            NameFilter::Named => "Named",
        };
        let name_text = format!(
            "{}{}",
            self.theme.fg(ThemeColor::Muted, "Name: "),
            self.theme.fg(ThemeColor::Accent, name_label)
        );

        let scope_text = if self.loading {
            let progress_text = match self.load_progress {
                Some((loaded, total)) => format!("{loaded}/{total}"),
                None => "...".to_owned(),
            };
            format!(
                "{}{}",
                self.theme.fg(ThemeColor::Muted, "○ Current Folder | "),
                self.theme.fg(ThemeColor::Accent, &format!("Loading {progress_text}"))
            )
        } else {
            match self.scope {
                SessionScope::Current => format!(
                    "{}{}",
                    self.theme.fg(ThemeColor::Accent, "◉ Current Folder"),
                    self.theme.fg(ThemeColor::Muted, " | ○ All")
                ),
                SessionScope::All => format!(
                    "{}{}",
                    self.theme.fg(ThemeColor::Muted, "○ Current Folder | "),
                    self.theme.fg(ThemeColor::Accent, "◉ All")
                ),
            }
        };

        let right_text = truncate_to_width(&format!("{scope_text}  {name_text}  {sort_text}"), width, "", false);
        let available_left = width.saturating_sub(visible_width(&right_text)).saturating_sub(1);
        let left = truncate_to_width(&left_text, available_left, "", false);
        let spacing = width
            .saturating_sub(visible_width(&left))
            .saturating_sub(visible_width(&right_text));

        let (hint_line1, hint_line2) = if self.confirming_delete_path.is_some() {
            let confirm_hint = format!(
                "Delete session? {} · {}",
                key_hint("tui.select.confirm", "confirm", &self.theme),
                key_hint("tui.select.cancel", "cancel", &self.theme)
            );
            (
                self.theme
                    .fg(ThemeColor::Error, &truncate_to_width(&confirm_hint, width, "…", false)),
                String::new(),
            )
        } else if let Some(status) = &self.status_message {
            let color = if status.is_error { ThemeColor::Error } else { ThemeColor::Accent };
            (
                self.theme.fg(color, &truncate_to_width(&status.message, width, "…", false)),
                String::new(),
            )
        } else {
            let path_state = if self.show_path { "(on)" } else { "(off)" };
            let sep = self.theme.fg(ThemeColor::Muted, " · ");
            let hint1 = format!(
                "{}{}{}",
                key_hint("tui.input.tab", "scope", &self.theme),
                sep,
                self.theme.fg(ThemeColor::Muted, "re:<pattern> regex · \"phrase\" exact")
            );
            let mut hint2_parts = vec![
                key_hint("app.session.toggleSort", "sort", &self.theme),
                key_hint("app.session.toggleNamedFilter", "named", &self.theme),
                key_hint("app.session.delete", "delete", &self.theme),
                key_hint("app.session.togglePath", &format!("path {path_state}"), &self.theme),
            ];
            if self.show_rename_hint {
                hint2_parts.push(key_hint("app.session.rename", "rename", &self.theme));
            }
            let hint2 = hint2_parts.join(&sep);
            (
                truncate_to_width(&hint1, width, "…", false),
                truncate_to_width(&hint2, width, "…", false),
            )
        };

        vec![
            format!("{left}{}{right_text}", " ".repeat(spacing)),
            hint_line1,
            hint_line2,
        ]
    }

    fn invalidate(&mut self) {}
}

/// A session tree node for hierarchical display.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeNode {
    pub session: SessionInfo,
    pub children: Vec<SessionTreeNode>,
    pub latest_activity: i64,
}

/// Flattened node for display with tree structure info.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatSessionNode {
    pub session: SessionInfo,
    pub depth: usize,
    pub is_last: bool,
    /// For each ancestor level, whether there are more siblings after it.
    pub ancestor_continues: Vec<bool>,
}

/// Build a tree from sessions based on parentSessionPath; roots sorted by modified desc.
pub fn build_session_tree(sessions: &[SessionInfo]) -> Vec<SessionTreeNode> {
    let mut keys: Vec<String> = Vec::with_capacity(sessions.len());
    let mut nodes: Vec<SessionTreeNode> = Vec::with_capacity(sessions.len());
    for session in sessions {
        keys.push(canonicalize_path(&session.path));
        nodes.push(SessionTreeNode {
            session: session.clone(),
            children: Vec::new(),
            latest_activity: session.modified_ms,
        });
    }

    let mut children_by_parent: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let parent_key = session.parent_session_path.as_deref().map(canonicalize_path);
        match parent_key.and_then(|key| keys.iter().position(|candidate| *candidate == key)) {
            Some(parent) => children_by_parent.entry(parent).or_default().push(index),
            None => roots.push(index),
        }
    }

    fn build(
        index: usize,
        nodes: &[SessionTreeNode],
        children_by_parent: &std::collections::BTreeMap<usize, Vec<usize>>,
    ) -> SessionTreeNode {
        let mut node = nodes[index].clone();
        node.children = children_by_parent
            .get(&index)
            .map(|children| children.iter().map(|child| build(*child, nodes, children_by_parent)).collect())
            .unwrap_or_default();
        node.latest_activity = node
            .children
            .iter()
            .fold(node.session.modified_ms, |latest, child| latest.max(child.latest_activity));
        node
    }

    fn sort_nodes(nodes: &mut [SessionTreeNode]) {
        nodes.sort_by_key(|node| std::cmp::Reverse(node.latest_activity));
        for node in nodes.iter_mut() {
            sort_nodes(&mut node.children);
        }
    }

    let mut tree: Vec<SessionTreeNode> =
        roots.iter().map(|root| build(*root, &nodes, &children_by_parent)).collect();
    sort_nodes(&mut tree);
    tree
}

/// Flatten a tree into a display list with tree structure metadata.
pub fn flatten_session_tree(roots: &[SessionTreeNode]) -> Vec<FlatSessionNode> {
    fn walk(
        node: &SessionTreeNode,
        depth: usize,
        ancestor_continues: &[bool],
        is_last: bool,
        result: &mut Vec<FlatSessionNode>,
    ) {
        result.push(FlatSessionNode {
            session: node.session.clone(),
            depth,
            is_last,
            ancestor_continues: ancestor_continues.to_vec(),
        });
        for (index, child) in node.children.iter().enumerate() {
            let child_is_last = index == node.children.len() - 1;
            // Only show a continuation line for non-root ancestors.
            let continues = depth > 0 && !is_last;
            let mut next = ancestor_continues.to_vec();
            next.push(continues);
            walk(child, depth + 1, &next, child_is_last, result);
        }
    }

    let mut result = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        walk(root, 0, &[], index == roots.len() - 1, &mut result);
    }
    result
}

/// senpi's buildTreePrefix.
pub fn build_tree_prefix(node: &FlatSessionNode) -> String {
    if node.depth == 0 {
        return String::new();
    }
    let parts: String = node
        .ancestor_continues
        .iter()
        .map(|continues| if *continues { "│  " } else { "   " })
        .collect();
    let branch = if node.is_last { "└─ " } else { "├─ " };
    format!("{parts}{branch}")
}

/// Custom session list component with multi-line items and search.
pub struct SessionList {
    theme: Theme,
    all_sessions: Vec<SessionInfo>,
    filtered_sessions: Vec<FlatSessionNode>,
    selected_index: usize,
    search_input: Input,
    show_cwd: bool,
    sort_mode: SortMode,
    name_filter: NameFilter,
    keybindings: Arc<KeybindingsManager>,
    show_path: bool,
    confirming_delete_path: Option<String>,
    current_session_canonical_path: Option<String>,
    max_visible: usize,
    home: Option<String>,
    focused: bool,
    pub on_select: Option<SessionPathHandler>,
    pub on_cancel: Option<VoidHandler>,
    pub on_toggle_scope: Option<VoidHandler>,
    pub on_toggle_sort: Option<VoidHandler>,
    pub on_toggle_name_filter: Option<VoidHandler>,
    pub on_toggle_path: Option<BoolHandler>,
    pub on_delete_confirmation_change: Option<OptionalPathHandler>,
    pub on_delete_session: Option<SessionPathHandler>,
    pub on_rename_session: Option<SessionPathHandler>,
    pub on_error: Option<SessionPathHandler>,
}

impl SessionList {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        sessions: Vec<SessionInfo>,
        show_cwd: bool,
        sort_mode: SortMode,
        name_filter: NameFilter,
        keybindings: Arc<KeybindingsManager>,
        current_session_file_path: Option<&str>,
        home: Option<String>,
    ) -> Self {
        let mut list = Self {
            theme: theme.clone(),
            all_sessions: sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
            search_input: Input::new(InputOptions::default()),
            show_cwd,
            sort_mode,
            name_filter,
            keybindings,
            show_path: false,
            confirming_delete_path: None,
            current_session_canonical_path: current_session_file_path.map(canonicalize_path),
            max_visible: 10,
            home,
            focused: false,
            on_select: None,
            on_cancel: None,
            on_toggle_scope: None,
            on_toggle_sort: None,
            on_toggle_name_filter: None,
            on_toggle_path: None,
            on_delete_confirmation_change: None,
            on_delete_session: None,
            on_rename_session: None,
            on_error: None,
        };
        list.filter_sessions("");
        list
    }

    pub fn selected_session_path(&self) -> Option<&str> {
        self.filtered_sessions
            .get(self.selected_index)
            .map(|node| node.session.path.as_str())
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn filtered_session_paths(&self) -> Vec<String> {
        self.filtered_sessions.iter().map(|node| node.session.path.clone()).collect()
    }

    pub fn show_path(&self) -> bool {
        self.show_path
    }

    pub fn confirming_delete_path(&self) -> Option<&str> {
        self.confirming_delete_path.as_deref()
    }

    pub fn set_sort_mode(&mut self, sort_mode: SortMode) {
        self.sort_mode = sort_mode;
        let query = self.search_input.get_value().to_owned();
        self.filter_sessions(&query);
    }

    pub fn set_name_filter(&mut self, name_filter: NameFilter) {
        self.name_filter = name_filter;
        let query = self.search_input.get_value().to_owned();
        self.filter_sessions(&query);
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>, show_cwd: bool) {
        self.all_sessions = sessions;
        self.show_cwd = show_cwd;
        let query = self.search_input.get_value().to_owned();
        self.filter_sessions(&query);
    }

    pub fn filter_sessions(&mut self, query: &str) {
        let trimmed = query.trim();
        let name_filtered: Vec<SessionInfo> = if self.name_filter == NameFilter::All {
            self.all_sessions.clone()
        } else {
            self.all_sessions
                .iter()
                .filter(|session| has_session_name(session))
                .cloned()
                .collect()
        };

        if self.sort_mode == SortMode::Threaded && trimmed.is_empty() {
            let roots = build_session_tree(&name_filtered);
            self.filtered_sessions = flatten_session_tree(&roots);
        } else {
            let filtered = filter_and_sort_sessions(&name_filtered, query, self.sort_mode, NameFilter::All);
            self.filtered_sessions = filtered
                .into_iter()
                .map(|session| FlatSessionNode {
                    session,
                    depth: 0,
                    is_last: true,
                    ancestor_continues: Vec::new(),
                })
                .collect();
        }
        self.selected_index = self.selected_index.min(self.filtered_sessions.len().saturating_sub(1));
    }

    fn set_confirming_delete_path(&mut self, path: Option<String>) {
        self.confirming_delete_path = path.clone();
        if let Some(callback) = &mut self.on_delete_confirmation_change {
            callback(path.as_deref());
        }
    }

    fn start_delete_confirmation_for_selected_session(&mut self) {
        let Some(selected) = self.filtered_sessions.get(self.selected_index) else {
            return;
        };
        if self.is_current_session_path(&selected.session.path) {
            if let Some(on_error) = &mut self.on_error {
                on_error("Cannot delete the currently active session");
            }
            return;
        }
        let path = selected.session.path.clone();
        self.set_confirming_delete_path(Some(path));
    }

    pub fn is_current_session_path(&self, path: &str) -> bool {
        let Some(current) = &self.current_session_canonical_path else {
            return false;
        };
        canonicalize_path(path) == *current
    }

    fn render_row(&self, node: &FlatSessionNode, index: usize, width: usize, now: i64) -> String {
        let session = &node.session;
        let is_selected = index == self.selected_index;
        let is_confirming_delete = self.confirming_delete_path.as_deref() == Some(session.path.as_str());
        let is_current = self.is_current_session_path(&session.path);

        let prefix = build_tree_prefix(node);

        let has_name = session.name.is_some();
        let display_text = session.name.clone().unwrap_or_else(|| session.first_message.clone());
        let normalized_message = display_text
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect::<String>()
            .trim()
            .to_owned();

        let age = format_session_date(session.modified_ms, now);
        let msg_count = session.message_count.to_string();
        let mut right_part = format!("{msg_count} {age}");
        if self.show_cwd && !session.cwd.is_empty() {
            right_part = format!("{} {right_part}", shorten_path(&session.cwd, self.home.as_deref()));
        }
        if self.show_path {
            right_part = format!("{} {right_part}", shorten_path(&session.path, self.home.as_deref()));
        }

        let cursor = if is_selected {
            self.theme.fg(ThemeColor::Accent, "› ")
        } else {
            "  ".to_owned()
        };

        let prefix_width = visible_width(&prefix);
        let right_width = visible_width(&right_part) + 2;
        let available_for_msg = (width as i64) - 2 - prefix_width as i64 - right_width as i64;

        let truncated_msg = truncate_to_width(&normalized_message, available_for_msg.max(10) as usize, "…", false);

        let message_color = if is_confirming_delete {
            Some(ThemeColor::Error)
        } else if is_current {
            Some(ThemeColor::Accent)
        } else if has_name {
            Some(ThemeColor::Warning)
        } else {
            None
        };
        let mut styled_msg = match message_color {
            Some(color) => self.theme.fg(color, &truncated_msg),
            None => truncated_msg,
        };
        if is_selected {
            styled_msg = self.theme.bold(&styled_msg);
        }

        let left_part = format!("{cursor}{}{styled_msg}", self.theme.fg(ThemeColor::Dim, &prefix));
        let left_width = visible_width(&left_part);
        let spacing = width.saturating_sub(left_width).saturating_sub(visible_width(&right_part)).max(1);
        let styled_right = self.theme.fg(
            if is_confirming_delete { ThemeColor::Error } else { ThemeColor::Dim },
            &right_part,
        );

        let mut line = format!("{left_part}{}{styled_right}", " ".repeat(spacing));
        if is_selected {
            line = self.theme.bg(ThemeBg::SelectedBg, &line);
        }
        truncate_to_width(&line, width, "", false)
    }
}

impl Component for SessionList {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        if self.filtered_sessions.is_empty() {
            let empty_message = if self.name_filter == NameFilter::Named {
                let toggle_key = key_text("app.session.toggleNamedFilter");
                if self.show_cwd {
                    format!("  No named sessions found. Press {toggle_key} to show all.")
                } else {
                    format!(
                        "  No named sessions in current folder. Press {toggle_key} to show all, or Tab to view all."
                    )
                }
            } else if self.show_cwd {
                "  No sessions found".to_owned()
            } else {
                "  No sessions in current folder. Press Tab to view all.".to_owned()
            };
            lines.push(
                self.theme
                    .fg(ThemeColor::Muted, &truncate_to_width(&empty_message, width, "…", false)),
            );
            return lines;
        }

        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible / 2)
            .min(self.filtered_sessions.len().saturating_sub(self.max_visible));
        let end_index = (start_index + self.max_visible).min(self.filtered_sessions.len());

        let now = now_ms();
        let nodes: Vec<FlatSessionNode> = self.filtered_sessions[start_index..end_index].to_vec();
        for (offset, node) in nodes.iter().enumerate() {
            lines.push(self.render_row(node, start_index + offset, width, now));
        }

        if start_index > 0 || end_index < self.filtered_sessions.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, self.filtered_sessions.len());
            lines.push(
                self.theme
                    .fg(ThemeColor::Muted, &truncate_to_width(&scroll_text, width, "", false)),
            );
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = Arc::clone(&self.keybindings);
        if self.confirming_delete_path.is_some() {
            if kb.matches(data, "tui.select.confirm") {
                let path = self.confirming_delete_path.take();
                self.set_confirming_delete_path(None);
                if let (Some(path), Some(callback)) = (path, &mut self.on_delete_session) {
                    callback(&path);
                }
                return;
            }
            if kb.matches(data, "tui.select.cancel") {
                self.set_confirming_delete_path(None);
                return;
            }
            return;
        }

        if kb.matches(data, "tui.input.tab") {
            if let Some(callback) = &mut self.on_toggle_scope {
                callback();
            }
            return;
        }
        if kb.matches(data, "app.session.toggleSort") {
            if let Some(callback) = &mut self.on_toggle_sort {
                callback();
            }
            return;
        }
        if kb.matches(data, "app.session.toggleNamedFilter") {
            if let Some(callback) = &mut self.on_toggle_name_filter {
                callback();
            }
            return;
        }
        if kb.matches(data, "app.session.togglePath") {
            self.show_path = !self.show_path;
            let show_path = self.show_path;
            if let Some(callback) = &mut self.on_toggle_path {
                callback(show_path);
            }
            return;
        }
        if kb.matches(data, "app.session.delete") {
            self.start_delete_confirmation_for_selected_session();
            return;
        }
        if kb.matches(data, "app.session.rename") {
            if let Some(selected) = self.filtered_sessions.get(self.selected_index) {
                let path = selected.session.path.clone();
                if let Some(callback) = &mut self.on_rename_session {
                    callback(&path);
                }
            }
            return;
        }
        if kb.matches(data, "app.session.deleteNoninvasive") {
            if !self.search_input.get_value().is_empty() {
                self.search_input.handle_input(data);
                let query = self.search_input.get_value().to_owned();
                self.filter_sessions(&query);
                return;
            }
            self.start_delete_confirmation_for_selected_session();
            return;
        }

        if kb.matches(data, "tui.select.up") {
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if kb.matches(data, "tui.select.down") {
            self.selected_index = (self.selected_index + 1).min(self.filtered_sessions.len().saturating_sub(1));
        } else if kb.matches(data, "tui.select.pageUp") {
            self.selected_index = self.selected_index.saturating_sub(self.max_visible);
        } else if kb.matches(data, "tui.select.pageDown") {
            self.selected_index =
                (self.selected_index + self.max_visible).min(self.filtered_sessions.len().saturating_sub(1));
        } else if kb.matches(data, "tui.select.confirm") {
            if let Some(selected) = self.filtered_sessions.get(self.selected_index) {
                let path = selected.session.path.clone();
                if let Some(callback) = &mut self.on_select {
                    callback(&path);
                }
            }
        } else if kb.matches(data, "tui.select.cancel") {
            if let Some(callback) = &mut self.on_cancel {
                callback();
            }
        } else {
            self.search_input.handle_input(data);
            let query = self.search_input.get_value().to_owned();
            self.filter_sessions(&query);
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
        self.search_input.set_focused(focused);
    }

    fn invalidate(&mut self) {
        self.search_input.invalidate();
    }
}

impl Focusable for SessionList {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.search_input.set_focused(value);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSelectorMode {
    List,
    Rename,
}

/// What the list asked the selector to do; drained by [`SessionSelectorComponent::handle_input`]
/// so the list's callbacks can reach the component's own handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    Select(String),
    Cancel,
}

pub type SessionSelectHandler = SessionPathHandler;
pub type SessionCancelHandler = VoidHandler;
pub type SessionRenameHandler = Box<dyn FnMut(&str, Option<&str>)>;
pub type RenderRequest = VoidHandler;

/// Component that renders a session selector.
pub struct SessionSelectorComponent {
    theme: Theme,
    session_list: Rc<RefCell<SessionList>>,
    header: SessionSelectorHeader,
    keybindings: Arc<KeybindingsManager>,
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    current_sessions: Option<Vec<SessionInfo>>,
    all_sessions: Option<Vec<SessionInfo>>,
    current_sessions_loader: SessionsLoader,
    all_sessions_loader: SessionsLoader,
    can_rename: bool,
    rename_input: Input,
    mode: SessionSelectorMode,
    rename_target_path: Option<String>,
    focused: bool,
    on_select: SessionSelectHandler,
    on_cancel: SessionCancelHandler,
    on_exit: SessionCancelHandler,
    request_render: RenderRequest,
    rename_session: Option<SessionRenameHandler>,
    pending: Rc<RefCell<Option<PendingAction>>>,
}

impl SessionSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        current_sessions_loader: SessionsLoader,
        all_sessions_loader: SessionsLoader,
        on_select: SessionSelectHandler,
        on_cancel: SessionCancelHandler,
        on_exit: SessionCancelHandler,
        request_render: RenderRequest,
        rename_session: Option<SessionRenameHandler>,
        show_rename_hint: Option<bool>,
        current_session_file_path: Option<&str>,
        home: Option<String>,
    ) -> Self {
        let can_rename = rename_session.is_some();
        let session_list = Rc::new(RefCell::new(SessionList::new(
            theme,
            Vec::new(),
            false,
            SortMode::Threaded,
            NameFilter::All,
            Arc::clone(&keybindings),
            current_session_file_path,
            home,
        )));
        let mut header =
            SessionSelectorHeader::new(theme, SessionScope::Current, SortMode::Threaded, NameFilter::All);
        header.set_show_rename_hint(show_rename_hint.unwrap_or(can_rename));
        let mut component = Self {
            theme: theme.clone(),
            session_list,
            header,
            keybindings,
            scope: SessionScope::Current,
            sort_mode: SortMode::Threaded,
            name_filter: NameFilter::All,
            current_sessions: None,
            all_sessions: None,
            current_sessions_loader,
            all_sessions_loader,
            can_rename,
            rename_input: Input::new(InputOptions::default()),
            mode: SessionSelectorMode::List,
            rename_target_path: None,
            focused: false,
            on_select,
            on_cancel,
            on_exit,
            request_render,
            rename_session,
            pending: Rc::new(RefCell::new(None)),
        };
        component.wire_list_events();
        component.load_scope(SessionScope::Current);
        component
    }

    pub fn session_list(&self) -> &Rc<RefCell<SessionList>> {
        &self.session_list
    }

    pub fn header(&self) -> &SessionSelectorHeader {
        &self.header
    }

    pub fn scope(&self) -> SessionScope {
        self.scope
    }

    pub fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    pub fn name_filter(&self) -> NameFilter {
        self.name_filter
    }

    pub fn mode(&self) -> SessionSelectorMode {
        self.mode
    }

    pub fn can_rename(&self) -> bool {
        self.can_rename
    }

    pub fn rename_input(&self) -> &Input {
        &self.rename_input
    }

    pub fn rename_input_mut(&mut self) -> &mut Input {
        &mut self.rename_input
    }

    /// senpi's onExit callback.
    pub fn exit(&mut self) {
        (self.on_exit)();
    }

    /// Wire the list's select/cancel callbacks to the pending-intent channel this component drains.
    fn wire_list_events(&mut self) {
        let pending_select = Rc::clone(&self.pending);
        let pending_cancel = Rc::clone(&self.pending);
        let mut list = self.session_list.borrow_mut();
        list.on_select = Some(Box::new(move |path| {
            *pending_select.borrow_mut() = Some(PendingAction::Select(path.to_owned()));
        }));
        list.on_cancel = Some(Box::new(move || {
            *pending_cancel.borrow_mut() = Some(PendingAction::Cancel);
        }));
    }

    fn drain_pending(&mut self) {
        let action = self.pending.borrow_mut().take();
        match action {
            Some(PendingAction::Select(path)) => (self.on_select)(&path),
            Some(PendingAction::Cancel) => (self.on_cancel)(),
            None => {}
        }
    }

    /// Load one scope through its loader (senpi's loadScope).
    pub fn load_scope(&mut self, scope: SessionScope) {
        let show_cwd = scope == SessionScope::All;
        self.header.set_scope(scope);
        self.header.set_loading(true);
        (self.request_render)();

        let loader = match scope {
            SessionScope::Current => &mut self.current_sessions_loader,
            SessionScope::All => &mut self.all_sessions_loader,
        };
        let sessions = loader(None);
        match scope {
            SessionScope::Current => self.current_sessions = Some(sessions.clone()),
            SessionScope::All => self.all_sessions = Some(sessions.clone()),
        }
        if scope != self.scope {
            return;
        }
        self.header.set_loading(false);
        self.session_list.borrow_mut().set_sessions(sessions, show_cwd);
        (self.request_render)();
    }

    /// Report a loader failure in the header (senpi's catch branch).
    pub fn report_load_error(&mut self, message: &str) {
        self.header.set_loading(false);
        self.header.set_status_message(Some(SessionSelectorStatusMessage {
            is_error: true,
            message: format!("Failed to load sessions: {message}"),
        }));
        (self.request_render)();
    }

    pub fn toggle_sort_mode(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Threaded => SortMode::Recent,
            SortMode::Recent => SortMode::Relevance,
            SortMode::Relevance => SortMode::Threaded,
        };
        self.header.set_sort_mode(self.sort_mode);
        self.session_list.borrow_mut().set_sort_mode(self.sort_mode);
        (self.request_render)();
    }

    pub fn toggle_name_filter(&mut self) {
        self.name_filter = match self.name_filter {
            NameFilter::All => NameFilter::Named,
            NameFilter::Named => NameFilter::All,
        };
        self.header.set_name_filter(self.name_filter);
        self.session_list.borrow_mut().set_name_filter(self.name_filter);
        (self.request_render)();
    }

    pub fn toggle_scope(&mut self) {
        match self.scope {
            SessionScope::Current => {
                self.scope = SessionScope::All;
                self.header.set_scope(self.scope);
                if let Some(sessions) = self.all_sessions.clone() {
                    self.header.set_loading(false);
                    self.session_list.borrow_mut().set_sessions(sessions, true);
                    (self.request_render)();
                    return;
                }
                self.load_scope(SessionScope::All);
            }
            SessionScope::All => {
                self.scope = SessionScope::Current;
                self.header.set_scope(self.scope);
                self.session_list
                    .borrow_mut()
                    .set_sessions(self.current_sessions.clone().unwrap_or_default(), false);
                (self.request_render)();
            }
        }
    }

    pub fn set_show_path(&mut self, show_path: bool) {
        self.header.set_show_path(show_path);
        (self.request_render)();
    }

    pub fn set_confirming_delete_path(&mut self, path: Option<String>) {
        self.header.set_confirming_delete_path(path.clone());
        self.session_list.borrow_mut().confirming_delete_path = path;
        (self.request_render)();
    }

    pub fn set_error_status(&mut self, message: &str) {
        self.header.set_status_message(Some(SessionSelectorStatusMessage {
            is_error: true,
            message: message.to_owned(),
        }));
        (self.request_render)();
    }

    pub fn enter_rename_mode(&mut self, session_path: &str, current_name: Option<&str>) {
        self.mode = SessionSelectorMode::Rename;
        self.rename_target_path = Some(session_path.to_owned());
        self.rename_input.set_value(current_name.unwrap_or(""));
        self.rename_input.set_focused(true);
        (self.request_render)();
    }

    pub fn exit_rename_mode(&mut self) {
        self.mode = SessionSelectorMode::List;
        self.rename_target_path = None;
        (self.request_render)();
    }

    pub fn confirm_rename(&mut self, value: &str) {
        let next = value.trim();
        if next.is_empty() {
            return;
        }
        let Some(target) = self.rename_target_path.clone() else {
            self.exit_rename_mode();
            return;
        };
        if let Some(rename_session) = &mut self.rename_session {
            rename_session(&target, Some(next));
        }
        self.exit_rename_mode();
    }
}

impl Component for SessionSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![
            String::new(),
            dynamic_border(&self.theme, ThemeColor::Accent, width),
            String::new(),
        ];
        match self.mode {
            SessionSelectorMode::List => {
                lines.extend(self.header.render(width));
                lines.push(String::new());
                lines.extend(self.session_list.borrow_mut().render(width));
            }
            SessionSelectorMode::Rename => {
                lines.extend(Text::with_padding(self.theme.bold("Rename Session"), 1, 0).render(width));
                lines.push(String::new());
                lines.extend(self.rename_input.render(width));
                lines.push(String::new());
                let hint = format!(
                    "{} to save · {} to cancel",
                    key_text("tui.select.confirm"),
                    key_text("tui.select.cancel")
                );
                lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &hint), 1, 0).render(width));
            }
        }
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Accent, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if self.mode == SessionSelectorMode::Rename {
            if self.keybindings.matches(data, "tui.select.cancel") {
                self.exit_rename_mode();
                return;
            }
            self.rename_input.handle_input(data);
            return;
        }
        self.session_list.borrow_mut().handle_input(data);
        self.drain_pending();
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.session_list.borrow_mut().set_focused(focused);
    }

    fn invalidate(&mut self) {
        self.session_list.borrow_mut().invalidate();
    }
}

impl Focusable for SessionSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.session_list.borrow_mut().set_focused(value);
    }
}
