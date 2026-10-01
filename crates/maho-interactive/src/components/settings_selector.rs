//! Port of components/settings-selector.ts.
//!
//! senpi reads the settings shapes from core/settings-manager.ts and core/settings-shapes.ts (plan
//! todo 21). The union types it renders are mirrored here as local enums with the same string
//! values, and the settings list theme comes from the interactive theme module.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use maho_tui::components::settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SettingsListTheme, SubmenuDone,
};
use maho_tui::components::text::Text;
use maho_tui::tui::Component;

use super::keybinding_hints::key_display_text;
use super::theme_selector::{dynamic_border, select_list_theme};
use crate::theme::theme::{Theme, ThemeColor, parse_auto_theme_setting};
use crate::theme::TerminalTheme;

/// pi-agent-core ThinkingLevel, including "off".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == value)
    }
}

/// settings-shapes.ts MermaidRenderingMode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidRenderingMode {
    Off,
    Final,
    Streaming,
}

impl MermaidRenderingMode {
    pub const ALL: [Self; 3] = [Self::Off, Self::Final, Self::Streaming];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Final => "final",
            Self::Streaming => "streaming",
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// settings-shapes.ts RendererTuiMode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMode {
    Regular,
    Fullscreen,
}

impl TuiMode {
    pub const ALL: [Self; 2] = [Self::Regular, Self::Fullscreen];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Fullscreen => "fullscreen",
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// settings-manager.ts FullscreenExitOutput.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenExitOutput {
    Transcript,
    ResumeHint,
}

impl FullscreenExitOutput {
    pub const ALL: [Self; 2] = [Self::Transcript, Self::ResumeHint];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::ResumeHint => "resume-hint",
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// settings-manager.ts DefaultProjectTrust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultProjectTrust {
    Ask,
    Always,
    Never,
}

impl DefaultProjectTrust {
    pub const ALL: [Self; 3] = [Self::Ask, Self::Always, Self::Never];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Always => "Always trust",
            Self::Never => "Never trust",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.label() == label)
    }
}

/// terminal-settings.ts TERMINAL_MOUSE_MODES.
pub const TERMINAL_MOUSE_MODES: [&str; 3] = ["off", "whilePending", "always"];

pub fn is_terminal_mouse_mode(value: &str) -> bool {
    TERMINAL_MOUSE_MODES.contains(&value)
}

/// http-dispatcher.ts HTTP_IDLE_TIMEOUT_CHOICES.
pub const HTTP_IDLE_TIMEOUT_CHOICES: [(&str, u64); 5] = [
    ("30 sec", 30_000),
    ("1 min", 60_000),
    ("2 min", 120_000),
    ("5 min", 300_000),
    ("disabled", 0),
];

/// http-dispatcher.ts formatHttpIdleTimeoutMs.
pub fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    match HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .find(|(_, ms)| *ms == timeout_ms)
    {
        Some((label, _)) => (*label).to_owned(),
        None => format!("{} sec", timeout_ms / 1000),
    }
}

/// settings-manager.ts WarningSettings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WarningSettings {
    /// Default true.
    pub anthropic_extra_usage: Option<bool>,
    /// Default false.
    pub off_recommended_model: Option<bool>,
}

const THINKING_DESCRIPTIONS: [(&str, &str); 7] = [
    ("off", "No reasoning"),
    ("minimal", "Very brief reasoning (~1k tokens)"),
    ("low", "Light reasoning (~2k tokens)"),
    ("medium", "Moderate reasoning (~8k tokens)"),
    ("high", "Deep reasoning (~16k tokens)"),
    ("xhigh", "Extended reasoning (~32k tokens or native xhigh effort)"),
    ("max", "Maximum reasoning"),
];

fn thinking_description(level: ThinkingLevel) -> &'static str {
    THINKING_DESCRIPTIONS
        .iter()
        .find(|(value, _)| *value == level.as_str())
        .map_or("", |(_, description)| *description)
}

const SETTINGS_SUBMENU_SELECT_LIST_LAYOUT: (usize, usize) = (12, 32);

/// settings-selector.ts SettingsConfig.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsConfig {
    pub auto_compact: bool,
    pub show_images: bool,
    pub image_width_cells: usize,
    pub auto_resize_images: bool,
    pub block_images: bool,
    pub enable_skill_commands: bool,
    pub steering_mode: String,
    pub follow_up_mode: String,
    pub transport: String,
    pub http_idle_timeout_ms: u64,
    pub thinking_level: ThinkingLevel,
    pub available_thinking_levels: Vec<ThinkingLevel>,
    pub current_theme: String,
    pub terminal_theme: crate::theme::theme::TerminalTheme,
    pub available_themes: Vec<String>,
    pub hide_thinking_block: bool,
    pub smooth_streaming: bool,
    pub smooth_streaming_fps: u32,
    pub mermaid_rendering_mode: MermaidRenderingMode,
    pub show_cache_miss_notices: bool,
    pub collapse_changelog: bool,
    pub enable_install_telemetry: bool,
    pub double_escape_action: String,
    pub tree_filter_mode: String,
    pub show_hardware_cursor: bool,
    pub editor_padding_x: usize,
    pub output_pad: u8,
    pub autocomplete_max_visible: usize,
    pub quiet_startup: bool,
    pub default_project_trust: DefaultProjectTrust,
    pub clear_on_shrink: bool,
    pub show_terminal_progress: bool,
    pub tui_mode: TuiMode,
    pub fullscreen_exit_output: FullscreenExitOutput,
    pub fullscreen_scrollbar: String,
    pub fullscreen_copy_on_select: bool,
    pub terminal_mouse: Option<String>,
    pub warnings: WarningSettings,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            auto_compact: true,
            show_images: true,
            image_width_cells: 80,
            auto_resize_images: true,
            block_images: false,
            enable_skill_commands: true,
            steering_mode: "one-at-a-time".to_owned(),
            follow_up_mode: "one-at-a-time".to_owned(),
            transport: "auto".to_owned(),
            http_idle_timeout_ms: 300_000,
            thinking_level: ThinkingLevel::Off,
            available_thinking_levels: ThinkingLevel::ALL.to_vec(),
            current_theme: "dark".to_owned(),
            terminal_theme: crate::theme::theme::TerminalTheme::Dark,
            available_themes: Vec::new(),
            hide_thinking_block: false,
            smooth_streaming: true,
            smooth_streaming_fps: 60,
            mermaid_rendering_mode: MermaidRenderingMode::Final,
            show_cache_miss_notices: true,
            collapse_changelog: false,
            enable_install_telemetry: true,
            double_escape_action: "tree".to_owned(),
            tree_filter_mode: "default".to_owned(),
            show_hardware_cursor: false,
            editor_padding_x: 0,
            output_pad: 1,
            autocomplete_max_visible: 10,
            quiet_startup: false,
            default_project_trust: DefaultProjectTrust::Ask,
            clear_on_shrink: true,
            show_terminal_progress: false,
            tui_mode: TuiMode::Regular,
            fullscreen_exit_output: FullscreenExitOutput::Transcript,
            fullscreen_scrollbar: "auto".to_owned(),
            fullscreen_copy_on_select: true,
            terminal_mouse: None,
            warnings: WarningSettings::default(),
        }
    }
}

/// settings-selector.ts SettingsCallbacks. Every callback is optional so a caller wires only the
/// settings it persists; an unwired callback is a no-op, exactly like an unhandled switch arm.
#[derive(Default)]
pub struct SettingsCallbacks {
    pub on_auto_compact_change: Option<Box<dyn FnMut(bool)>>,
    pub on_show_images_change: Option<Box<dyn FnMut(bool)>>,
    pub on_image_width_cells_change: Option<Box<dyn FnMut(usize)>>,
    pub on_auto_resize_images_change: Option<Box<dyn FnMut(bool)>>,
    pub on_block_images_change: Option<Box<dyn FnMut(bool)>>,
    pub on_enable_skill_commands_change: Option<Box<dyn FnMut(bool)>>,
    pub on_steering_mode_change: Option<Box<dyn FnMut(String)>>,
    pub on_follow_up_mode_change: Option<Box<dyn FnMut(String)>>,
    pub on_transport_change: Option<Box<dyn FnMut(String)>>,
    pub on_http_idle_timeout_ms_change: Option<Box<dyn FnMut(u64)>>,
    pub on_thinking_level_change: Option<Box<dyn FnMut(ThinkingLevel)>>,
    pub on_theme_change: Option<Box<dyn FnMut(String)>>,
    pub on_theme_preview: Option<Box<dyn FnMut(String)>>,
    pub on_hide_thinking_block_change: Option<Box<dyn FnMut(bool)>>,
    pub on_smooth_streaming_change: Option<Box<dyn FnMut(bool)>>,
    pub on_smooth_streaming_fps_change: Option<Box<dyn FnMut(u32)>>,
    pub on_mermaid_rendering_mode_change: Option<Box<dyn FnMut(MermaidRenderingMode)>>,
    pub on_show_cache_miss_notices_change: Option<Box<dyn FnMut(bool)>>,
    pub on_collapse_changelog_change: Option<Box<dyn FnMut(bool)>>,
    pub on_enable_install_telemetry_change: Option<Box<dyn FnMut(bool)>>,
    pub on_double_escape_action_change: Option<Box<dyn FnMut(String)>>,
    pub on_tree_filter_mode_change: Option<Box<dyn FnMut(String)>>,
    pub on_show_hardware_cursor_change: Option<Box<dyn FnMut(bool)>>,
    pub on_editor_padding_x_change: Option<Box<dyn FnMut(usize)>>,
    pub on_output_pad_change: Option<Box<dyn FnMut(u8)>>,
    pub on_autocomplete_max_visible_change: Option<Box<dyn FnMut(usize)>>,
    pub on_quiet_startup_change: Option<Box<dyn FnMut(bool)>>,
    pub on_default_project_trust_change: Option<Box<dyn FnMut(DefaultProjectTrust)>>,
    pub on_clear_on_shrink_change: Option<Box<dyn FnMut(bool)>>,
    pub on_show_terminal_progress_change: Option<Box<dyn FnMut(bool)>>,
    pub on_tui_mode_change: Option<Box<dyn FnMut(TuiMode)>>,
    pub on_terminal_mouse_change: Option<Box<dyn FnMut(String)>>,
    pub on_fullscreen_exit_output_change: Option<Box<dyn FnMut(FullscreenExitOutput)>>,
    pub on_fullscreen_scrollbar_change: Option<Box<dyn FnMut(String)>>,
    pub on_fullscreen_copy_on_select_change: Option<Box<dyn FnMut(bool)>>,
    pub on_warnings_change: Option<Box<dyn FnMut(WarningSettings)>>,
    pub on_cancel: Option<Box<dyn FnMut()>>,
}

pub type SelectHandler = Box<dyn FnMut(&str)>;
pub type VoidHandler = Box<dyn FnMut()>;

/// A submenu for selecting from a list of options (senpi's SelectSubmenu).
pub struct SelectSubmenu {
    theme: Theme,
    select_list: SelectList,
    hint: String,
    title: String,
    description: Option<String>,
}

impl SelectSubmenu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        title: &str,
        description: &str,
        options: Vec<SelectItem>,
        current_value: &str,
        on_select: SelectHandler,
        on_cancel: VoidHandler,
        on_selection_change: Option<SelectHandler>,
    ) -> Self {
        let mut select_list = SelectList::new(
            options.clone(),
            10.min(options.len()),
            select_list_theme(theme),
            SelectListLayoutOptions {
                min_primary_column_width: Some(SETTINGS_SUBMENU_SELECT_LIST_LAYOUT.0),
                max_primary_column_width: Some(SETTINGS_SUBMENU_SELECT_LIST_LAYOUT.1),
                truncate_primary: None,
            },
        );
        if let Some(index) = options.iter().position(|option| option.value == current_value) {
            select_list.set_selected_index(index);
        }
        let mut on_select = on_select;
        select_list.on_select = Some(Box::new(move |item: &SelectItem| on_select(&item.value)));
        select_list.on_cancel = Some(on_cancel);
        if let Some(mut on_selection_change) = on_selection_change {
            select_list.on_selection_change =
                Some(Box::new(move |item: &SelectItem| on_selection_change(&item.value)));
        }
        Self {
            theme: theme.clone(),
            select_list,
            hint: "  Enter to select · Esc to go back".to_owned(),
            title: title.to_owned(),
            description: (!description.is_empty()).then(|| description.to_owned()),
        }
    }
}

impl Component for SelectSubmenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(
            Text::with_padding(self.theme.bold(&self.theme.fg(ThemeColor::Accent, &self.title)), 0, 0)
                .render(width),
        );
        if let Some(description) = self.description.clone() {
            lines.push(String::new());
            lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &description), 0, 0).render(width));
        }
        lines.push(String::new());
        lines.extend(self.select_list.render(width));
        lines.push(String::new());
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Dim, &self.hint), 0, 0).render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.select_list.handle_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.select_list.invalidate();
    }
}

/// senpi's WarningSettingsSubmenu.
pub struct WarningSettingsSubmenu {
    settings_list: SettingsList,
}

impl WarningSettingsSubmenu {
    pub fn new(
        theme: &Theme,
        warnings: WarningSettings,
        on_change: Box<dyn FnMut(WarningSettings)>,
        on_cancel: Box<dyn FnMut()>,
    ) -> Self {
        let state = warnings;
        let items = vec![SettingItem {
            id: "anthropic-extra-usage".to_owned(),
            label: "Anthropic extra usage".to_owned(),
            description: Some(
                "Warn when Anthropic subscription auth may use paid extra usage".to_owned(),
            ),
            current_value: if state.anthropic_extra_usage.unwrap_or(true) { "true" } else { "false" }
                .to_owned(),
            values: Some(vec!["true".to_owned(), "false".to_owned()]),
            submenu: None,
        }];
        let mut settings_list = SettingsList::new(
            items.clone(),
            10.min(items.len()),
            settings_list_theme(theme),
            SettingsListOptions::default(),
        );
        let mut on_change = on_change;
        settings_list.on_change = Some(Box::new(move |id, new_value| {
            if id == "anthropic-extra-usage" {
                on_change(WarningSettings {
                    anthropic_extra_usage: Some(new_value == "true"),
                    ..state
                });
            }
        }));
        settings_list.on_cancel = Some(on_cancel);
        Self { settings_list }
    }
}

impl Component for WarningSettingsSubmenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.settings_list.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.settings_list.handle_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.settings_list.invalidate();
    }
}

/// settings-selector.ts themeItems.
pub fn theme_items(available_themes: &[String], current_theme: &str) -> Vec<SelectItem> {
    available_themes
        .iter()
        .map(|name| SelectItem {
            value: name.clone(),
            label: format!("{} {name}", if name == current_theme { "✓" } else { " " }),
            description: None,
        })
        .collect()
}

/// settings-selector.ts AUTOMATIC_THEME_VALUE.
pub const AUTOMATIC_THEME_VALUE: &str = "/";

/// settings-selector.ts singleModeThemeItems.
pub fn single_mode_theme_items(available_themes: &[String], current_theme: &str) -> Vec<SelectItem> {
    let mut items = vec![SelectItem {
        value: AUTOMATIC_THEME_VALUE.to_owned(),
        label: "  Automatic".to_owned(),
        description: Some("Use separate themes for light and dark terminal appearance".to_owned()),
    }];
    items.extend(theme_items(available_themes, current_theme));
    items
}

fn preferred_theme(available_themes: &[String], preferred: Option<&str>, fallback: &str) -> String {
    if let Some(preferred) = preferred
        && available_themes.iter().any(|name| name == preferred)
    {
        return preferred.to_owned();
    }
    if available_themes.iter().any(|name| name == fallback) {
        return fallback.to_owned();
    }
    available_themes.first().cloned().unwrap_or_else(|| fallback.to_owned())
}

fn default_automatic_themes(current_theme_setting: &str, available_themes: &[String]) -> (String, String) {
    if let Some((light, dark)) = parse_auto_theme_setting(Some(current_theme_setting)) {
        return (light.to_owned(), dark.to_owned());
    }
    let current_fixed = (!current_theme_setting.contains('/')).then_some(current_theme_setting);
    let theme_name = preferred_theme(available_themes, current_fixed, "dark");
    (theme_name.clone(), theme_name)
}

/// senpi's getSettingsListTheme().
pub fn settings_list_theme(theme: &Theme) -> SettingsListTheme {
    let label_theme = theme.clone();
    let value_theme = theme.clone();
    let description_theme = theme.clone();
    let hint_theme = theme.clone();
    SettingsListTheme {
        label: Rc::new(move |text: &str, selected: bool| {
            if selected { label_theme.fg(ThemeColor::Accent, text) } else { text.to_owned() }
        }),
        value: Rc::new(move |text: &str, selected: bool| {
            if selected {
                value_theme.fg(ThemeColor::Accent, text)
            } else {
                value_theme.fg(ThemeColor::Muted, text)
            }
        }),
        description: Rc::new(move |text: &str| description_theme.fg(ThemeColor::Dim, text)),
        cursor: theme.fg(ThemeColor::Accent, "→ "),
        hint: Rc::new(move |text: &str| hint_theme.fg(ThemeColor::Dim, text)),
    }
}

/// The state senpi's ThemeSubmenu keeps between renders.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeSubmenuState {
    pub mode: ThemeSubmenuMode,
    pub single_theme: String,
    pub light_theme: String,
    pub dark_theme: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSubmenuMode {
    Single,
    Automatic,
}

enum ThemeSubmenuContent {
    Single(Box<SelectSubmenu>),
    Automatic(Box<SettingsList>),
}

/// senpi's ThemeSubmenu.
pub struct ThemeSubmenu {
    theme: Theme,
    callbacks: Rc<RefCell<SettingsCallbacks>>,
    available_themes: Vec<String>,
    terminal_theme: TerminalTheme,
    done: SubmenuDone,
    original_theme_setting: String,
    state: Rc<RefCell<ThemeSubmenuState>>,
    /// Set by the automatic menu's Change mode item; drained in handle_input.
    mode_switch: Rc<std::cell::Cell<bool>>,
    content: ThemeSubmenuContent,
}

impl ThemeSubmenu {
    pub fn new(
        theme: &Theme,
        current_theme_setting: &str,
        terminal_theme: TerminalTheme,
        available_themes: Vec<String>,
        callbacks: Rc<RefCell<SettingsCallbacks>>,
        done: SubmenuDone,
    ) -> Self {
        let auto_theme = parse_auto_theme_setting(Some(current_theme_setting));
        let automatic = default_automatic_themes(current_theme_setting, &available_themes);
        let fixed_theme = if auto_theme.is_some() || current_theme_setting.contains('/') {
            None
        } else {
            Some(current_theme_setting)
        };
        let active_automatic = if terminal_theme == TerminalTheme::Light {
            automatic.0.clone()
        } else {
            automatic.1.clone()
        };
        let mode = if auto_theme.is_some() { ThemeSubmenuMode::Automatic } else { ThemeSubmenuMode::Single };
        let single_theme = preferred_theme(
            &available_themes,
            fixed_theme.or(if auto_theme.is_some() { Some(active_automatic.as_str()) } else { None }),
            "dark",
        );
        let mut submenu = Self {
            theme: theme.clone(),
            callbacks,
            available_themes,
            terminal_theme,
            done,
            original_theme_setting: current_theme_setting.to_owned(),
            state: Rc::new(RefCell::new(ThemeSubmenuState {
                mode,
                single_theme,
                light_theme: automatic.0,
                dark_theme: automatic.1,
            })),
            mode_switch: Rc::new(std::cell::Cell::new(false)),
            content: ThemeSubmenuContent::Single(Box::new(SelectSubmenu::new(
                theme,
                "Theme",
                "",
                Vec::new(),
                "",
                Box::new(|_| {}),
                Box::new(|| {}),
                None,
            ))),
        };
        if mode == ThemeSubmenuMode::Automatic {
            submenu.show_automatic_menu();
        } else {
            submenu.show_single_menu();
        }
        submenu
    }

    pub fn state(&self) -> &Rc<RefCell<ThemeSubmenuState>> {
        &self.state
    }

    pub fn content_is_single(&self) -> bool {
        matches!(self.content, ThemeSubmenuContent::Single(_))
    }

    fn active_automatic_theme(&self) -> String {
        let state = self.state.borrow();
        if self.terminal_theme == TerminalTheme::Light {
            state.light_theme.clone()
        } else {
            state.dark_theme.clone()
        }
    }

    pub fn automatic_theme_setting(&self) -> String {
        let state = self.state.borrow();
        format!("{}/{}", state.light_theme, state.dark_theme)
    }

    pub fn theme_setting(&self) -> String {
        if self.state.borrow().mode == ThemeSubmenuMode::Automatic {
            self.automatic_theme_setting()
        } else {
            self.state.borrow().single_theme.clone()
        }
    }

    fn preview(&self, theme_name: &str) {
        if let Some(callback) = &mut self.callbacks.borrow_mut().on_theme_preview {
            callback(theme_name.to_owned());
        }
    }

    pub fn apply(&self, theme_setting: &str) {
        (self.done)(Some(theme_setting.to_owned()), None);
    }

    /// senpi's cancel(): restore the preview and close without committing.
    pub fn cancel(&self) {
        self.preview(&self.original_theme_setting.clone());
        (self.done)(None, None);
    }

    fn show_single_menu(&mut self) {
        self.state.borrow_mut().mode = ThemeSubmenuMode::Single;
        let single_theme = self.state.borrow().single_theme.clone();
        let state_for_select = Rc::clone(&self.state);
        let callbacks_for_preview = Rc::clone(&self.callbacks);
        let done_for_select = Rc::clone(&self.done);
        let state_for_preview = Rc::clone(&self.state);
        let menu = SelectSubmenu::new(
            &self.theme,
            "Theme",
            "Select a theme, or choose Automatic to follow terminal appearance.",
            single_mode_theme_items(&self.available_themes, &single_theme),
            &single_theme,
            Box::new(move |value| {
                if value == AUTOMATIC_THEME_VALUE {
                    state_for_select.borrow_mut().mode = ThemeSubmenuMode::Automatic;
                } else {
                    state_for_select.borrow_mut().single_theme = value.to_owned();
                    done_for_select(Some(value.to_owned()), None);
                }
            }),
            Box::new(|| {}),
            Some(Box::new(move |value| {
                let preview = if value == AUTOMATIC_THEME_VALUE {
                    let state = state_for_preview.borrow();
                    format!("{}/{}", state.light_theme, state.dark_theme)
                } else {
                    value.to_owned()
                };
                if let Some(callback) = &mut callbacks_for_preview.borrow_mut().on_theme_preview {
                    callback(preview);
                }
            })),
        );
        self.content = ThemeSubmenuContent::Single(Box::new(menu));
    }

    fn show_automatic_menu(&mut self) {
        self.state.borrow_mut().mode = ThemeSubmenuMode::Automatic;
        let state = Rc::clone(&self.state);
        let themes = self.available_themes.clone();
        let theme = self.theme.clone();
        let mode_switch = Rc::new(std::cell::Cell::new(false));
        let mode_switch_for_change = Rc::clone(&mode_switch);

        let theme_picker = |title: &'static str,
                            description: &'static str,
                            current_value: String,
                            is_light: bool| {
            let themes = themes.clone();
            let theme = theme.clone();
            let state = Rc::clone(&state);
            SettingItem {
                id: if is_light { "light-theme" } else { "dark-theme" }.to_owned(),
                label: if is_light { "Light theme" } else { "Dark theme" }.to_owned(),
                description: Some(description.to_owned()),
                current_value,
                values: None,
                submenu: Some(Rc::new(move |current_value: &str, done: SubmenuDone| {
                    let state = Rc::clone(&state);
                    let done_for_select = Rc::clone(&done);
                    let component: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(SelectSubmenu::new(
                        &theme,
                        title,
                        description,
                        theme_items(&themes, current_value),
                        current_value,
                        Box::new(move |value| {
                            let mut state = state.borrow_mut();
                            if is_light {
                                state.light_theme = value.to_owned();
                            } else {
                                state.dark_theme = value.to_owned();
                            }
                            done_for_select(Some(value.to_owned()), None);
                        }),
                        Box::new(move || done(None, None)),
                        None,
                    )));
                    component
                })),
            }
        };

        let items = vec![
            theme_picker(
                "Light Theme",
                "Select the theme to use for light terminal appearance",
                self.state.borrow().light_theme.clone(),
                true,
            ),
            theme_picker(
                "Dark Theme",
                "Select the theme to use for dark terminal appearance",
                self.state.borrow().dark_theme.clone(),
                false,
            ),
            SettingItem {
                id: "apply".to_owned(),
                label: "Apply".to_owned(),
                description: Some("Save and go back".to_owned()),
                current_value: "save and go back".to_owned(),
                values: Some(vec!["save and go back".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "single-mode".to_owned(),
                label: "Change mode".to_owned(),
                description: Some("Switch to one theme for light and dark".to_owned()),
                current_value: "switch to single theme".to_owned(),
                values: Some(vec!["switch to single theme".to_owned()]),
                submenu: None,
            },
        ];

        let mut settings_list = SettingsList::new(
            items.clone(),
            10.min(items.len()),
            settings_list_theme(&self.theme),
            SettingsListOptions::default(),
        );
        let state_for_change = Rc::clone(&self.state);
        let done_for_change = Rc::clone(&self.done);
        let callbacks_for_change = Rc::clone(&self.callbacks);
        settings_list.on_change = Some(Box::new(move |id, _new_value| match id {
            "single-mode" => mode_switch_for_change.set(true),
            "apply" => {
                let state = state_for_change.borrow();
                done_for_change(Some(format!("{}/{}", state.light_theme, state.dark_theme)), None);
            }
            _ => {
                if let Some(callback) = &mut callbacks_for_change.borrow_mut().on_theme_preview {
                    let state = state_for_change.borrow();
                    callback(format!("{}/{}", state.light_theme, state.dark_theme));
                }
            }
        }));
        settings_list.on_cancel = Some(Box::new({
            let done = Rc::clone(&self.done);
            move || done(None, None)
        }));
        self.content = ThemeSubmenuContent::Automatic(Box::new(settings_list));
        self.mode_switch = mode_switch;
    }
}

impl Component for ThemeSubmenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        match &mut self.content {
            ThemeSubmenuContent::Single(menu) => menu.render(width),
            ThemeSubmenuContent::Automatic(list) => list.render(width),
        }
    }

    fn handle_input(&mut self, data: &str) {
        match &mut self.content {
            ThemeSubmenuContent::Single(menu) => menu.handle_input(data),
            ThemeSubmenuContent::Automatic(list) => list.handle_input(data),
        }
        if self.mode_switch.replace(false) {
            let active = self.active_automatic_theme();
            self.state.borrow_mut().single_theme = active.clone();
            self.preview(&active);
            self.show_single_menu();
            return;
        }
        if self.state.borrow().mode == ThemeSubmenuMode::Automatic && self.content_is_single() {
            let setting = self.theme_setting();
            self.preview(&setting);
            self.show_automatic_menu();
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        match &mut self.content {
            ThemeSubmenuContent::Single(menu) => menu.invalidate(),
            ThemeSubmenuContent::Automatic(list) => list.invalidate(),
        }
    }
}

/// Main settings selector component.
pub struct SettingsSelectorComponent {
    theme: Theme,
    settings_list: SettingsList,
}

impl SettingsSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        config: SettingsConfig,
        callbacks: SettingsCallbacks,
        supports_images: bool,
    ) -> Self {
        let callbacks = Rc::new(RefCell::new(callbacks));
        let follow_up_key = key_display_text("app.message.followUp");
        let current_warnings = Rc::new(std::cell::Cell::new(config.warnings));

        let mut items: Vec<SettingItem> = vec![
            SettingItem {
                id: "autocompact".to_owned(),
                label: "Auto-compact".to_owned(),
                description: Some("Automatically compact context when it gets too large".to_owned()),
                current_value: config.auto_compact.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "steering-mode".to_owned(),
                label: "Steering mode".to_owned(),
                description: Some(
                    "Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.".to_owned(),
                ),
                current_value: config.steering_mode.clone(),
                values: Some(vec!["one-at-a-time".to_owned(), "all".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "follow-up-mode".to_owned(),
                label: "Follow-up mode".to_owned(),
                description: Some(format!(
                    "{follow_up_key} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once."
                )),
                current_value: config.follow_up_mode.clone(),
                values: Some(vec!["one-at-a-time".to_owned(), "all".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "transport".to_owned(),
                label: "Transport".to_owned(),
                description: Some(
                    "Preferred transport for providers that support multiple transports".to_owned(),
                ),
                current_value: config.transport.clone(),
                values: Some(vec![
                    "sse".to_owned(),
                    "websocket".to_owned(),
                    "websocket-cached".to_owned(),
                    "auto".to_owned(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "http-idle-timeout".to_owned(),
                label: "HTTP idle timeout".to_owned(),
                description: Some(
                    "Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes.".to_owned(),
                ),
                current_value: format_http_idle_timeout_ms(config.http_idle_timeout_ms),
                values: Some(HTTP_IDLE_TIMEOUT_CHOICES.iter().map(|(label, _)| (*label).to_owned()).collect()),
                submenu: None,
            },
            SettingItem {
                id: "hide-thinking".to_owned(),
                label: "Hide thinking".to_owned(),
                description: Some("Hide thinking blocks in assistant responses".to_owned()),
                current_value: config.hide_thinking_block.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "smooth-streaming".to_owned(),
                label: "Smooth streaming".to_owned(),
                description: Some("Reveal streamed assistant text at a steady pace".to_owned()),
                current_value: config.smooth_streaming.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "streaming-fps".to_owned(),
                label: "Streaming fps".to_owned(),
                description: Some("Maximum frame rate for smooth streaming".to_owned()),
                current_value: config.smooth_streaming_fps.to_string(),
                values: Some(vec!["30".to_owned(), "60".to_owned(), "90".to_owned(), "120".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "mermaid-rendering".to_owned(),
                label: "Mermaid diagrams".to_owned(),
                description: Some("Render Mermaid code blocks as Unicode diagrams".to_owned()),
                current_value: config.mermaid_rendering_mode.as_str().to_owned(),
                values: Some(vec!["off".to_owned(), "final".to_owned(), "streaming".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "cache-miss-notices".to_owned(),
                label: "Cache miss notices".to_owned(),
                description: Some(
                    "Show transcript notices for cache costs and provider recovery diagnostics".to_owned(),
                ),
                current_value: config.show_cache_miss_notices.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "collapse-changelog".to_owned(),
                label: "Collapse changelog".to_owned(),
                description: Some("Show condensed changelog after updates".to_owned()),
                current_value: config.collapse_changelog.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "quiet-startup".to_owned(),
                label: "Quiet startup".to_owned(),
                description: Some("Disable verbose printing at startup".to_owned()),
                current_value: config.quiet_startup.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "install-telemetry".to_owned(),
                label: "Install telemetry".to_owned(),
                description: Some(
                    "Send an anonymous version/update ping after changelog-detected updates".to_owned(),
                ),
                current_value: config.enable_install_telemetry.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "default-project-trust".to_owned(),
                label: "Default project trust".to_owned(),
                description: Some(
                    "Fallback behavior when no extension or saved trust decision decides project trust".to_owned(),
                ),
                current_value: config.default_project_trust.label().to_owned(),
                values: Some(DefaultProjectTrust::ALL.iter().map(|value| value.label().to_owned()).collect()),
                submenu: None,
            },
            SettingItem {
                id: "double-escape-action".to_owned(),
                label: "Double-escape action".to_owned(),
                description: Some("Action when pressing Escape twice with empty editor".to_owned()),
                current_value: config.double_escape_action.clone(),
                values: Some(vec!["tree".to_owned(), "fork".to_owned(), "none".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "tree-filter-mode".to_owned(),
                label: "Tree filter mode".to_owned(),
                description: Some("Default filter when opening /tree".to_owned()),
                current_value: config.tree_filter_mode.clone(),
                values: Some(vec![
                    "default".to_owned(),
                    "no-tools".to_owned(),
                    "user-only".to_owned(),
                    "labeled-only".to_owned(),
                    "all".to_owned(),
                ]),
                submenu: None,
            },
            SettingItem {
                id: "warnings".to_owned(),
                label: "Warnings".to_owned(),
                description: Some("Enable or disable individual warnings".to_owned()),
                current_value: "configure".to_owned(),
                values: None,
                submenu: Some(Rc::new({
                    let theme = theme.clone();
                    let callbacks = Rc::clone(&callbacks);
                    let current_warnings = Rc::clone(&current_warnings);
                    move |_current_value: &str, done: SubmenuDone| {
                        let component: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(
                            WarningSettingsSubmenu::new(
                                &theme,
                                current_warnings.get(),
                                Box::new({
                                    let callbacks = Rc::clone(&callbacks);
                                    let current_warnings = Rc::clone(&current_warnings);
                                    move |warnings| {
                                        current_warnings.set(warnings);
                                        if let Some(callback) =
                                            &mut callbacks.borrow_mut().on_warnings_change
                                        {
                                            callback(warnings);
                                        }
                                    }
                                }),
                                {
                                    let done = Rc::clone(&done);
                                    Box::new(move || done(None, None))
                                },
                            ),
                        ));
                        component
                    }
                })),
            },
            SettingItem {
                id: "thinking".to_owned(),
                label: "Thinking level".to_owned(),
                description: Some("Reasoning depth for thinking-capable models".to_owned()),
                current_value: config.thinking_level.as_str().to_owned(),
                values: None,
                submenu: Some(Rc::new({
                    let theme = theme.clone();
                    let callbacks = Rc::clone(&callbacks);
                    let levels = config.available_thinking_levels.clone();
                    move |current_value: &str, done: SubmenuDone| {
                        let options: Vec<SelectItem> = levels
                            .iter()
                            .map(|level| SelectItem {
                                value: level.as_str().to_owned(),
                                label: level.as_str().to_owned(),
                                description: Some(thinking_description(*level).to_owned()),
                            })
                            .collect();
                        let callbacks_for_select = Rc::clone(&callbacks);
                        let done_for_select = Rc::clone(&done);
                        let component: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(
                            SelectSubmenu::new(
                                &theme,
                                "Thinking Level",
                                "Select reasoning depth for thinking-capable models",
                                options,
                                current_value,
                                Box::new(move |value| {
                                    if let Some(level) = ThinkingLevel::from_name(value)
                                        && let Some(callback) =
                                            &mut callbacks_for_select.borrow_mut().on_thinking_level_change
                                    {
                                        callback(level);
                                    }
                                    done_for_select(Some(value.to_owned()), None);
                                }),
                                {
                                    let done = Rc::clone(&done);
                                    Box::new(move || done(None, None))
                                },
                                None,
                            ),
                        ));
                        component
                    }
                })),
            },
            SettingItem {
                id: "tui-mode".to_owned(),
                label: "TUI mode".to_owned(),
                description: Some("Interface layout; fullscreen mode is experimental".to_owned()),
                current_value: config.tui_mode.as_str().to_owned(),
                values: Some(vec!["regular".to_owned(), "fullscreen".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "terminal-mouse".to_owned(),
                label: "Terminal mouse".to_owned(),
                description: Some(
                    "Capture while questions are pending, always, or off (also disables fullscreen mouse)".to_owned(),
                ),
                current_value: config.terminal_mouse.clone().unwrap_or_else(|| "whilePending".to_owned()),
                values: Some(TERMINAL_MOUSE_MODES.iter().map(|mode| (*mode).to_owned()).collect()),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-exit-output".to_owned(),
                label: "Fullscreen exit output".to_owned(),
                description: Some(
                    "Print the transcript or only a session resume hint when exiting fullscreen mode".to_owned(),
                ),
                current_value: config.fullscreen_exit_output.as_str().to_owned(),
                values: Some(vec!["transcript".to_owned(), "resume-hint".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-scrollbar".to_owned(),
                label: "Fullscreen scrollbar".to_owned(),
                description: Some(
                    "Scrollbar behavior in fullscreen mode; has no effect in regular mode".to_owned(),
                ),
                current_value: config.fullscreen_scrollbar.clone(),
                values: Some(vec!["auto".to_owned(), "always".to_owned(), "hidden".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "fullscreen-copy-on-select".to_owned(),
                label: "Fullscreen copy on select".to_owned(),
                description: Some(
                    "Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X".to_owned(),
                ),
                current_value: config.fullscreen_copy_on_select.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
            SettingItem {
                id: "theme".to_owned(),
                label: "Theme".to_owned(),
                description: Some("Color theme for the interface".to_owned()),
                current_value: config.current_theme.clone(),
                values: None,
                submenu: Some(Rc::new({
                    let theme = theme.clone();
                    let callbacks = Rc::clone(&callbacks);
                    let available_themes = config.available_themes.clone();
                    let terminal_theme = config.terminal_theme;
                    move |current_value: &str, done: SubmenuDone| {
                        let component: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(
                            ThemeSubmenu::new(
                                &theme,
                                current_value,
                                terminal_theme,
                                available_themes.clone(),
                                Rc::clone(&callbacks),
                                done,
                            ),
                        ));
                        component
                    }
                })),
            },
        ];

        if supports_images {
            items.insert(
                1,
                SettingItem {
                    id: "show-images".to_owned(),
                    label: "Show images".to_owned(),
                    description: Some("Render images inline in terminal".to_owned()),
                    current_value: config.show_images.to_string(),
                    values: Some(vec!["true".to_owned(), "false".to_owned()]),
                    submenu: None,
                },
            );
            items.insert(
                2,
                SettingItem {
                    id: "image-width-cells".to_owned(),
                    label: "Image width".to_owned(),
                    description: Some("Preferred inline image width in terminal cells".to_owned()),
                    current_value: config.image_width_cells.to_string(),
                    values: Some(vec!["60".to_owned(), "80".to_owned(), "120".to_owned()]),
                    submenu: None,
                },
            );
        }

        items.insert(
            if supports_images { 3 } else { 1 },
            SettingItem {
                id: "auto-resize-images".to_owned(),
                label: "Auto-resize images".to_owned(),
                description: Some(
                    "Resize large images to 2000x2000 max for better model compatibility".to_owned(),
                ),
                current_value: config.auto_resize_images.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
        );

        let auto_resize_index = item_index(&items, "auto-resize-images");
        items.insert(
            auto_resize_index + 1,
            SettingItem {
                id: "block-images".to_owned(),
                label: "Block images".to_owned(),
                description: Some("Prevent images from being sent to LLM providers".to_owned()),
                current_value: config.block_images.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
        );
        let block_images_index = item_index(&items, "block-images");
        items.insert(
            block_images_index + 1,
            SettingItem {
                id: "skill-commands".to_owned(),
                label: "Skill commands".to_owned(),
                description: Some("Register skills as /skill:name commands".to_owned()),
                current_value: config.enable_skill_commands.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
        );
        let skill_commands_index = item_index(&items, "skill-commands");
        items.insert(
            skill_commands_index + 1,
            SettingItem {
                id: "show-hardware-cursor".to_owned(),
                label: "Show hardware cursor".to_owned(),
                description: Some(
                    "Show the terminal cursor while still positioning it for IME support".to_owned(),
                ),
                current_value: config.show_hardware_cursor.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
        );
        let hardware_cursor_index = item_index(&items, "show-hardware-cursor");
        items.insert(
            hardware_cursor_index + 1,
            SettingItem {
                id: "editor-padding".to_owned(),
                label: "Editor padding".to_owned(),
                description: Some("Horizontal padding for input editor (0-3)".to_owned()),
                current_value: config.editor_padding_x.to_string(),
                values: Some(vec!["0".to_owned(), "1".to_owned(), "2".to_owned(), "3".to_owned()]),
                submenu: None,
            },
        );
        let editor_padding_index = item_index(&items, "editor-padding");
        items.insert(
            editor_padding_index + 1,
            SettingItem {
                id: "output-padding".to_owned(),
                label: "Output padding".to_owned(),
                description: Some(
                    "Horizontal padding for user messages, assistant messages, and thinking".to_owned(),
                ),
                current_value: config.output_pad.to_string(),
                values: Some(vec!["0".to_owned(), "1".to_owned()]),
                submenu: None,
            },
        );
        let output_padding_index = item_index(&items, "output-padding");
        items.insert(
            output_padding_index + 1,
            SettingItem {
                id: "autocomplete-max-visible".to_owned(),
                label: "Autocomplete max items".to_owned(),
                description: Some("Max visible items in autocomplete dropdown (3-20)".to_owned()),
                current_value: config.autocomplete_max_visible.to_string(),
                values: Some(vec![
                    "3".to_owned(),
                    "5".to_owned(),
                    "7".to_owned(),
                    "10".to_owned(),
                    "15".to_owned(),
                    "20".to_owned(),
                ]),
                submenu: None,
            },
        );
        let autocomplete_index = item_index(&items, "autocomplete-max-visible");
        items.insert(
            autocomplete_index + 1,
            SettingItem {
                id: "clear-on-shrink".to_owned(),
                label: "Clear on shrink".to_owned(),
                description: Some("Clear empty rows when content shrinks (may cause flicker)".to_owned()),
                current_value: config.clear_on_shrink.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
        );
        let clear_on_shrink_index = item_index(&items, "clear-on-shrink");
        items.insert(
            clear_on_shrink_index + 1,
            SettingItem {
                id: "terminal-progress".to_owned(),
                label: "Terminal progress".to_owned(),
                description: Some("Show OSC 9;4 progress indicators in the terminal tab bar".to_owned()),
                current_value: config.show_terminal_progress.to_string(),
                values: Some(vec!["true".to_owned(), "false".to_owned()]),
                submenu: None,
            },
        );

        let mut settings_list = SettingsList::new(
            items.clone(),
            10,
            settings_list_theme(theme),
            SettingsListOptions { enable_search: true },
        );
        let callbacks_for_change = Rc::clone(&callbacks);
        settings_list.on_change = Some(Box::new(move |id, new_value| {
            let mut callbacks = callbacks_for_change.borrow_mut();
            match id {
                "autocompact" => call(&mut callbacks.on_auto_compact_change, new_value == "true"),
                "show-images" => call(&mut callbacks.on_show_images_change, new_value == "true"),
                "image-width-cells" => {
                    if let Ok(value) = new_value.parse::<usize>() {
                        call(&mut callbacks.on_image_width_cells_change, value);
                    }
                }
                "auto-resize-images" => {
                    call(&mut callbacks.on_auto_resize_images_change, new_value == "true");
                }
                "block-images" => call(&mut callbacks.on_block_images_change, new_value == "true"),
                "skill-commands" => {
                    call(&mut callbacks.on_enable_skill_commands_change, new_value == "true");
                }
                "steering-mode" => call(&mut callbacks.on_steering_mode_change, new_value.to_owned()),
                "follow-up-mode" => call(&mut callbacks.on_follow_up_mode_change, new_value.to_owned()),
                "transport" => call(&mut callbacks.on_transport_change, new_value.to_owned()),
                "http-idle-timeout" => {
                    if let Some((_, timeout_ms)) = HTTP_IDLE_TIMEOUT_CHOICES
                        .iter()
                        .find(|(label, _)| *label == new_value)
                    {
                        call(&mut callbacks.on_http_idle_timeout_ms_change, *timeout_ms);
                    }
                }
                "hide-thinking" => call(&mut callbacks.on_hide_thinking_block_change, new_value == "true"),
                "smooth-streaming" => call(&mut callbacks.on_smooth_streaming_change, new_value == "true"),
                "streaming-fps" => {
                    if let Ok(value) = new_value.parse::<u32>() {
                        call(&mut callbacks.on_smooth_streaming_fps_change, value);
                    }
                }
                "mermaid-rendering" => {
                    if let Some(mode) = MermaidRenderingMode::from_name(new_value) {
                        call(&mut callbacks.on_mermaid_rendering_mode_change, mode);
                    }
                }
                "cache-miss-notices" => {
                    call(&mut callbacks.on_show_cache_miss_notices_change, new_value == "true");
                }
                "collapse-changelog" => {
                    call(&mut callbacks.on_collapse_changelog_change, new_value == "true");
                }
                "quiet-startup" => call(&mut callbacks.on_quiet_startup_change, new_value == "true"),
                "install-telemetry" => {
                    call(&mut callbacks.on_enable_install_telemetry_change, new_value == "true");
                }
                "default-project-trust" => {
                    if let Some(value) = DefaultProjectTrust::from_label(new_value) {
                        call(&mut callbacks.on_default_project_trust_change, value);
                    }
                }
                "double-escape-action" => {
                    call(&mut callbacks.on_double_escape_action_change, new_value.to_owned());
                }
                "tree-filter-mode" => {
                    call(&mut callbacks.on_tree_filter_mode_change, new_value.to_owned());
                }
                "show-hardware-cursor" => {
                    call(&mut callbacks.on_show_hardware_cursor_change, new_value == "true");
                }
                "editor-padding" => {
                    if let Ok(value) = new_value.parse::<usize>() {
                        call(&mut callbacks.on_editor_padding_x_change, value);
                    }
                }
                "output-padding" => call(
                    &mut callbacks.on_output_pad_change,
                    if new_value == "0" { 0 } else { 1 },
                ),
                "autocomplete-max-visible" => {
                    if let Ok(value) = new_value.parse::<usize>() {
                        call(&mut callbacks.on_autocomplete_max_visible_change, value);
                    }
                }
                "clear-on-shrink" => call(&mut callbacks.on_clear_on_shrink_change, new_value == "true"),
                "terminal-progress" => {
                    call(&mut callbacks.on_show_terminal_progress_change, new_value == "true");
                }
                "terminal-mouse" if is_terminal_mouse_mode(new_value) => {
                    call(&mut callbacks.on_terminal_mouse_change, new_value.to_owned());
                }
                "tui-mode" => {
                    if let Some(mode) = TuiMode::from_name(new_value) {
                        call(&mut callbacks.on_tui_mode_change, mode);
                    }
                }
                "fullscreen-exit-output" => {
                    if let Some(mode) = FullscreenExitOutput::from_name(new_value) {
                        call(&mut callbacks.on_fullscreen_exit_output_change, mode);
                    }
                }
                "fullscreen-scrollbar" => {
                    call(&mut callbacks.on_fullscreen_scrollbar_change, new_value.to_owned());
                }
                "fullscreen-copy-on-select" => {
                    call(&mut callbacks.on_fullscreen_copy_on_select_change, new_value == "true");
                }
                "theme" => call(&mut callbacks.on_theme_change, new_value.to_owned()),
                _ => {}
            }
        }));
        let callbacks_for_cancel = Rc::clone(&callbacks);
        settings_list.on_cancel = Some(Box::new(move || {
            if let Some(callback) = &mut callbacks_for_cancel.borrow_mut().on_cancel {
                callback();
            }
        }));

        Self {
            theme: theme.clone(),
            settings_list,
        }
    }

    pub fn settings_list(&self) -> &SettingsList {
        &self.settings_list
    }

    pub fn settings_list_mut(&mut self) -> &mut SettingsList {
        &mut self.settings_list
    }
}

fn call<T>(handler: &mut Option<Box<dyn FnMut(T)>>, value: T) {
    if let Some(handler) = handler {
        handler(value);
    }
}

fn item_index(items: &[SettingItem], id: &str) -> usize {
    items.iter().position(|item| item.id == id).unwrap_or(0)
}

impl Component for SettingsSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width)];
        lines.extend(self.settings_list.render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.settings_list.handle_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.settings_list.invalidate();
    }
}
