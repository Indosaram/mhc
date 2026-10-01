//! Port of `components/bash-execution.ts`.
//!
//! senpi's `Loader` runs its own timer; the native loader advances on host ticks, so the loader is
//! driven from `BashExecutionComponent::tick` exactly as `tool-execution` drives its spinner.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, truncate_tail};
use maho_tui::components::loader::Loader;
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, Container};
use maho_tui::utils::strip_terminal_sequences;

use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_hint, key_text};
use super::visual_truncate::truncate_to_visual_lines;
use crate::theme::{Theme, ThemeColor};

const PREVIEW_LINES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BashStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

fn highlight_bash_command(command: &str, theme: &Theme) -> String {
    crate::theme::highlight_code(theme, &command.replace('\r', "").replace('\t', "   "), Some("bash")).join("\n")
}

fn format_command_header(command: &str, dim: bool, theme: &Theme) -> String {
    let prefix = theme.fg(if dim { ThemeColor::Dim } else { ThemeColor::BashMode }, &theme.bold("$ "));
    let command_display =
        if dim { theme.fg(ThemeColor::Dim, command) } else { highlight_bash_command(command, theme) };
    prefix + &command_display
}

struct CachedPreview {
    styled_input: String,
    theme: Theme,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Component for CachedPreview {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.cached_lines.is_none() || self.cached_width != Some(width) {
            let result = truncate_to_visual_lines(&self.styled_input, PREVIEW_LINES, width, 1);
            self.cached_lines = Some(result.visual_lines);
            self.cached_width = Some(width);
        }
        let _ = &self.theme;
        self.cached_lines.clone().unwrap_or_default()
    }
    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_lines = None;
    }
}

pub struct BashExecutionComponent {
    command: String,
    output_lines: Vec<String>,
    status: BashStatus,
    exit_code: Option<i32>,
    loader: Rc<RefCell<Loader>>,
    truncation_result: Option<TruncationResult>,
    full_output_path: Option<String>,
    expanded: bool,
    exclude_from_context: bool,
    theme: Theme,
    content: Container,
}

impl BashExecutionComponent {
    pub fn new(command: &str, exclude_from_context: bool, theme: Theme) -> Self {
        let loader_theme = theme.clone();
        let loader_message_theme = theme.clone();
        let loader = Rc::new(RefCell::new(Loader::new(
            Rc::new(move |spinner: &str| {
                loader_theme.fg(if exclude_from_context { ThemeColor::Dim } else { ThemeColor::BashMode }, spinner)
            }),
            Rc::new(move |text: &str| loader_message_theme.fg(ThemeColor::Muted, text)),
            format!("Running... ({} to cancel)", key_text("tui.select.cancel")),
            None,
            0,
        )));
        Self {
            command: command.to_owned(),
            output_lines: Vec::new(),
            status: BashStatus::Running,
            exit_code: None,
            loader,
            truncation_result: None,
            full_output_path: None,
            expanded: false,
            exclude_from_context,
            theme,
            content: Container::new(),
        }
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.update_display();
    }

    pub fn append_output(&mut self, chunk: &str) {
        let clean = strip_terminal_sequences(chunk).replace("\r\n", "\n").replace('\r', "\n");
        let new_lines: Vec<&str> = clean.split('\n').collect();
        if !self.output_lines.is_empty() && !new_lines.is_empty() {
            let last = self.output_lines.len() - 1;
            self.output_lines[last].push_str(new_lines[0]);
            self.output_lines.extend(new_lines[1..].iter().map(|line| (*line).to_owned()));
        } else {
            self.output_lines.extend(new_lines.iter().map(|line| (*line).to_owned()));
        }
        self.update_display();
    }

    pub fn set_complete(
        &mut self,
        exit_code: Option<i32>,
        cancelled: bool,
        truncation_result: Option<TruncationResult>,
        full_output_path: Option<String>,
    ) {
        self.exit_code = exit_code;
        self.status = if cancelled {
            BashStatus::Cancelled
        } else if exit_code.is_some_and(|code| code != 0) {
            BashStatus::Error
        } else {
            BashStatus::Complete
        };
        self.truncation_result = truncation_result;
        self.full_output_path = full_output_path;
        self.loader.borrow_mut().stop();
        self.update_display();
    }

    pub fn tick(&mut self, now_ms: u64) -> bool {
        if self.status == BashStatus::Running { self.loader.borrow_mut().tick(now_ms) } else { false }
    }

    fn update_display(&mut self) {
        let full_output = self.output_lines.join("\n");
        let context_truncation = truncate_tail(
            &full_output,
            TruncationOptions { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES },
        );

        let available_lines: Vec<String> = if context_truncation.content.is_empty() {
            Vec::new()
        } else {
            context_truncation.content.split('\n').map(str::to_owned).collect()
        };
        let preview_start = available_lines.len().saturating_sub(PREVIEW_LINES);
        let preview_logical_lines: Vec<String> = available_lines[preview_start..].to_vec();
        let hidden_line_count = available_lines.len() - preview_logical_lines.len();

        self.content.clear();

        self.content.add_child(Rc::new(RefCell::new(Text::with_padding(
            format_command_header(&self.command, false, &self.theme),
            1,
            0,
        ))));

        if !available_lines.is_empty() {
            if self.expanded {
                let display_text = available_lines
                    .iter()
                    .map(|line| self.theme.fg(ThemeColor::Muted, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.content.add_child(Rc::new(RefCell::new(Text::with_padding(format!("\n{display_text}"), 1, 0))));
            } else {
                let styled_output = preview_logical_lines
                    .iter()
                    .map(|line| self.theme.fg(ThemeColor::Muted, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.content.add_child(Rc::new(RefCell::new(CachedPreview {
                    styled_input: format!("\n{styled_output}"),
                    theme: self.theme.clone(),
                    cached_width: None,
                    cached_lines: None,
                })));
            }
        }

        if self.status == BashStatus::Running {
            self.content.add_child(Rc::clone(&self.loader) as Rc<RefCell<dyn Component>>);
        } else {
            let mut status_parts: Vec<String> = Vec::new();
            if hidden_line_count > 0 {
                if self.expanded {
                    status_parts.push(format!(
                        "{}{}{}",
                        self.theme.fg(ThemeColor::Muted, "("),
                        key_hint("app.tools.expand", "to collapse", &self.theme),
                        self.theme.fg(ThemeColor::Muted, ")")
                    ));
                } else {
                    status_parts.push(format!(
                        "{}{}{}",
                        self.theme.fg(ThemeColor::Muted, &format!("... {hidden_line_count} more lines (")),
                        key_hint("app.tools.expand", "to expand", &self.theme),
                        self.theme.fg(ThemeColor::Muted, ")")
                    ));
                }
            }

            match self.status {
                BashStatus::Cancelled => status_parts.push(self.theme.fg(ThemeColor::Warning, "(cancelled)")),
                BashStatus::Error => {
                    status_parts
                        .push(self.theme.fg(ThemeColor::Error, &format!("(exit {})", self.exit_code.unwrap_or(0))));
                }
                _ => {}
            }

            let was_truncated = self.truncation_result.as_ref().is_some_and(|result| result.truncated)
                || context_truncation.truncated;
            if was_truncated && let Some(path) = &self.full_output_path {
                status_parts.push(
                    self.theme.fg(ThemeColor::Warning, &format!("Output truncated. Full output: {path}")),
                );
            }

            if !status_parts.is_empty() {
                self.content
                    .add_child(Rc::new(RefCell::new(Text::with_padding(format!("\n{}", status_parts.join("\n")), 1, 0))));
            }
        }
    }

    pub fn get_output(&self) -> String {
        self.output_lines.join("\n")
    }

    pub fn get_command(&self) -> &str {
        &self.command
    }
}

impl Component for BashExecutionComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let color_key = if self.exclude_from_context { ThemeColor::Dim } else { ThemeColor::BashMode };
        let border_theme = self.theme.clone();
        let mut lines: Vec<String> = Vec::new();
        lines.extend(Spacer::new(1).render(width));
        lines.extend(
            DynamicBorder::with_color(Rc::new(move |text| border_theme.fg(color_key, text))).render(width),
        );
        lines.extend(self.content.render(width));
        let border_theme = self.theme.clone();
        lines.extend(
            DynamicBorder::with_color(Rc::new(move |text| border_theme.fg(color_key, text))).render(width),
        );
        lines
    }
    fn invalidate(&mut self) {
        self.content.invalidate();
        self.update_display();
    }
    fn dispose(&mut self) {
        self.content.dispose();
    }
}
