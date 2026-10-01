//! Port of senpi `packages/coding-agent/src/modes/interactive/components/first-time-setup.ts`.
//!
//! The dialog composes `DynamicBorder` (todo 32) and reads a module-global theme; here it renders
//! the identical themed rule privately and takes the theme instance explicitly.

use std::cell::RefCell;
use std::rc::Rc;

use maho_core::config::app_name;
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, Container};

use crate::theme::{TerminalTheme, Theme, ThemeColor};

use super::ask_user_answer_key::{key_hint, raw_key_hint};

pub struct FirstTimeSetupResult {
    pub theme: TerminalTheme,
    pub share_analytics: bool,
}

pub struct FirstTimeSetupOptions {
    pub detected_theme: TerminalTheme,
    pub theme: Theme,
    pub on_theme_preview: Box<dyn FnMut(TerminalTheme)>,
    pub on_submit: Box<dyn FnMut(FirstTimeSetupResult)>,
    pub on_cancel: Box<dyn FnMut()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Theme,
    Analytics,
}

struct ThemeOption {
    value: TerminalTheme,
    label: &'static str,
}

const THEME_OPTIONS: &[ThemeOption] = &[
    ThemeOption {
        value: TerminalTheme::Dark,
        label: "Dark",
    },
    ThemeOption {
        value: TerminalTheme::Light,
        label: "Light",
    },
];

struct AnalyticsOption {
    value: bool,
    label: &'static str,
}

const ANALYTICS_OPTIONS: &[AnalyticsOption] = &[
    AnalyticsOption {
        value: true,
        label: "Share anonymous usage data",
    },
    AnalyticsOption {
        value: false,
        label: "Don't share",
    },
];

const SETUP_LOGO_LINES: [&str; 4] = ["██████", "██  ██", "████  ██", "██    ██"];

struct SetupBorder {
    theme: Theme,
}

impl Component for SetupBorder {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![self.theme.fg(ThemeColor::Border, &"─".repeat(width.max(1)))]
    }
}

pub struct FirstTimeSetupComponent {
    step: Step,
    theme_index: usize,
    analytics_index: usize,
    theme: Theme,
    on_theme_preview: Box<dyn FnMut(TerminalTheme)>,
    on_submit: Box<dyn FnMut(FirstTimeSetupResult)>,
    on_cancel: Box<dyn FnMut()>,
    root: Container,
}

impl FirstTimeSetupComponent {
    pub fn new(options: FirstTimeSetupOptions) -> Self {
        let theme_index = THEME_OPTIONS
            .iter()
            .position(|option| option.value == options.detected_theme)
            .unwrap_or(0);
        let mut component = Self {
            step: Step::Theme,
            theme_index,
            analytics_index: 0,
            theme: options.theme,
            on_theme_preview: options.on_theme_preview,
            on_submit: options.on_submit,
            on_cancel: options.on_cancel,
            root: Container::new(),
        };
        component.update();
        component
    }

    pub fn step_is_theme(&self) -> bool {
        self.step == Step::Theme
    }

    fn add_text(&mut self, text: String) {
        self.root
            .add_child(Rc::new(RefCell::new(Text::with_padding(text, 1, 0))));
    }

    fn add_option_list(&mut self, labels: &[&str], selected_index: usize) {
        for (index, label) in labels.iter().enumerate() {
            let is_selected = index == selected_index;
            let prefix = if is_selected {
                self.theme.fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = if is_selected {
                self.theme.fg(ThemeColor::Accent, label)
            } else {
                self.theme.fg(ThemeColor::Text, label)
            };
            self.add_text(format!("{prefix}{label}"));
        }
    }

    fn update(&mut self) {
        self.root.clear();
        let theme = self.theme.clone();
        self.root
            .add_child(Rc::new(RefCell::new(SetupBorder { theme: theme.clone() })));
        self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        self.add_text(theme.fg(ThemeColor::Accent, &SETUP_LOGO_LINES.join("\n")));
        self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        self.add_text(theme.fg(
            ThemeColor::Accent,
            &theme.bold(&format!(
                "Welcome to {}, the minimal coding agent.",
                app_name()
            )),
        ));
        self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        if self.step == Step::Theme {
            self.add_text(theme.fg(ThemeColor::Text, "Pick a theme."));
            self.add_text(theme.fg(
                ThemeColor::Muted,
                &format!(
                    "Detected system appearance: {}",
                    theme_name(self.detected_theme())
                ),
            ));
            self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
            let labels: Vec<&str> = THEME_OPTIONS.iter().map(|option| option.label).collect();
            self.add_option_list(&labels, self.theme_index);
        } else {
            self.add_text(theme.fg(
                ThemeColor::Text,
                "Opt-in to anonymous usage data sharing?",
            ));
            self.add_text(theme.fg(
                ThemeColor::Muted,
                "Opting in stores a tracking identifier in settings.json and enables anonymous\nusage analytics. This helps us to better debug, reproduce, and resolve issues\nand bugs within Pi. You can observe what is shared using /privacy and make\nchanges anytime in settings.json.",
            ));
            self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
            let labels: Vec<&str> = ANALYTICS_OPTIONS.iter().map(|option| option.label).collect();
            self.add_option_list(&labels, self.analytics_index);
        }

        self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        let confirm = if self.step == Step::Theme {
            "continue"
        } else {
            "finish"
        };
        self.add_text(format!(
            "{}{}{}",
            raw_key_hint(&theme, "↑↓", "navigate"),
            "  ",
            key_hint(&theme, "tui.select.confirm", confirm)
        ) + &format!("  {}", key_hint(&theme, "tui.select.cancel", "skip setup")));
        self.root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        let theme = self.theme.clone();
        self.root
            .add_child(Rc::new(RefCell::new(SetupBorder { theme })));
    }

    fn detected_theme(&self) -> TerminalTheme {
        THEME_OPTIONS[self.theme_index].value
    }

    fn move_selection(&mut self, delta: isize) {
        if self.step == Step::Theme {
            let next = clamp_index(self.theme_index, delta, THEME_OPTIONS.len());
            if next != self.theme_index {
                self.theme_index = next;
                (self.on_theme_preview)(THEME_OPTIONS[self.theme_index].value);
            }
        } else {
            self.analytics_index = clamp_index(self.analytics_index, delta, ANALYTICS_OPTIONS.len());
        }
        self.update();
    }
}

fn clamp_index(current: usize, delta: isize, len: usize) -> usize {
    (current as isize + delta).clamp(0, len as isize - 1) as usize
}

fn theme_name(theme: TerminalTheme) -> &'static str {
    match theme {
        TerminalTheme::Dark => "dark",
        TerminalTheme::Light => "light",
    }
}

impl Component for FirstTimeSetupComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.root.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            self.move_selection(-1);
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            self.move_selection(1);
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            if self.step == Step::Theme {
                self.step = Step::Analytics;
                self.update();
            } else {
                (self.on_submit)(FirstTimeSetupResult {
                    theme: THEME_OPTIONS[self.theme_index].value,
                    share_analytics: ANALYTICS_OPTIONS[self.analytics_index].value,
                });
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}
