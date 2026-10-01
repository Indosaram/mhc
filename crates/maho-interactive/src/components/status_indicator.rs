//! Port of status-indicator.ts. Countdown and loader timers use explicit host ticks.
use crate::theme::{Theme, ThemeColor};
use maho_tui::{
    components::loader::{Loader, LoaderIndicatorOptions},
    tui::Component,
    utils::{slice_by_column, truncate_to_width, visible_width},
};
use std::rc::Rc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusIndicatorKind {
    Working,
    Retry,
    Compaction,
    BranchSummary,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionStatusReason {
    Manual,
    Threshold,
    Overflow,
    PrePrompt,
    Branch,
    Extension,
}
pub struct StatusIndicator {
    pub kind: StatusIndicatorKind,
    pub loader: Loader,
    theme: Theme,
    progress: String,
    compact_label: String,
    retry: Option<(u64, usize, usize, bool, String)>,
}
impl StatusIndicator {
    pub fn new(
        kind: StatusIndicatorKind,
        message: &str,
        theme: Theme,
        indicator: Option<LoaderIndicatorOptions>,
        now: u64,
    ) -> Self {
        let spinner_theme = theme.clone();
        let text_theme = theme.clone();
        let color = if kind == StatusIndicatorKind::Retry {
            ThemeColor::Warning
        } else {
            ThemeColor::Accent
        };
        let mut indicator = indicator;
        if kind == StatusIndicatorKind::Retry
            && let Some(i) = &mut indicator
            && i.indicator_formatter.is_none()
        {
            let t = theme.clone();
            i.indicator_formatter = Some(Rc::new(move |s, _| t.fg(ThemeColor::Warning, s)));
        }
        Self {
            kind,
            loader: Loader::new(
                Rc::new(move |s| spinner_theme.fg(color, s)),
                Rc::new(move |s| text_theme.fg(ThemeColor::Muted, s)),
                message,
                indicator,
                now,
            ),
            theme,
            progress: String::new(),
            compact_label: String::new(),
            retry: None,
        }
    }
    pub fn working(message: &str, theme: Theme, now: u64) -> Self {
        Self::new(StatusIndicatorKind::Working, message, theme, None, now)
    }
    pub fn branch_summary(key: &str, theme: Theme, now: u64) -> Self {
        Self::new(
            StatusIndicatorKind::BranchSummary,
            &format!("Summarizing branch... ({key} to cancel)"),
            theme,
            None,
            now,
        )
    }
    pub fn compaction(reason: CompactionStatusReason, key: &str, theme: Theme, now: u64) -> Self {
        let label = match reason {
            CompactionStatusReason::Manual
            | CompactionStatusReason::Branch
            | CompactionStatusReason::Extension => "Compacting context...",
            CompactionStatusReason::Threshold => "Auto-compacting...",
            CompactionStatusReason::Overflow => "Context overflow detected, compacting...",
            CompactionStatusReason::PrePrompt => "Compacting before next prompt...",
        };
        let mut s = Self::new(
            StatusIndicatorKind::Compaction,
            &format!("{label} ({key} to cancel)"),
            theme,
            None,
            now,
        );
        s.compact_label = format!("Compacting... ({key} to cancel)");
        s
    }
    pub fn retry(
        attempt: usize,
        max: usize,
        delay: u64,
        trouble: bool,
        key: &str,
        theme: Theme,
        now: u64,
    ) -> Self {
        let message = retry_message(attempt, max, delay.div_ceil(1000), trouble, key);
        let mut s = Self::new(StatusIndicatorKind::Retry, &message, theme, None, now);
        s.retry = Some((now.saturating_add(delay), attempt, max, trouble, key.into()));
        s
    }
    pub fn tick(&mut self, now: u64) -> bool {
        if let Some((deadline, attempt, max, trouble, key)) = &self.retry {
            let seconds = deadline.saturating_sub(now).div_ceil(1000);
            self.loader
                .set_message(retry_message(*attempt, *max, seconds, *trouble, key), now);
            if *trouble {
                self.compact_label = format!(
                    "Retrying {attempt}/{max} {} ({key} cancel)",
                    if seconds > 0 {
                        format!("in {seconds}s")
                    } else {
                        "now".into()
                    }
                );
            }
        }
        self.loader.tick(now)
    }
    pub fn set_progress_text(&mut self, text: &str, now: u64) {
        if self.kind == StatusIndicatorKind::Compaction {
            if self.progress.is_empty() && !text.is_empty() {
                self.loader.set_message(&self.compact_label, now);
            }
            self.progress = text.into();
        }
    }
    pub fn render_in_border(&mut self, width: usize) -> String {
        if self.kind == StatusIndicatorKind::Compaction {
            return truncate_to_width(
                self.render(width).first().map_or("", |s| s.trim()),
                width,
                "",
                false,
            );
        }
        let lines = self.loader.render(width + 2);
        let line = lines.get(1).map_or("", String::as_str);
        let line = if self.kind == StatusIndicatorKind::Retry
            && !self.compact_label.is_empty()
            && lines.len() > 2
        {
            format!(
                "{} {}",
                self.render_spinner_in_border(width),
                self.theme.fg(ThemeColor::Muted, &self.compact_label)
            )
        } else {
            line.strip_prefix(' ').unwrap_or(line).trim_end().into()
        };
        truncate_to_width(&line, width, "", false)
    }
    pub fn render_spinner_in_border(&mut self, width: usize) -> String {
        let line = self
            .loader
            .render(width + 2)
            .get(1)
            .cloned()
            .unwrap_or_default();
        truncate_to_width(
            line.trim_start().split(' ').next().unwrap_or(""),
            width,
            "",
            false,
        )
    }
}
fn retry_message(attempt: usize, max: usize, seconds: u64, trouble: bool, key: &str) -> String {
    if trouble {
        format!(
            "The model provider may be having trouble. Retrying... ({attempt}/{max}, {}; {key} to cancel)",
            if seconds > 0 {
                format!("in {seconds}s")
            } else {
                "now".into()
            }
        )
    } else {
        format!("Retrying ({attempt}/{max}) in {seconds}s... ({key} to cancel)")
    }
}
impl Component for StatusIndicator {
    fn invalidate(&mut self) {
        self.loader.invalidate();
    }
    fn dispose(&mut self) {
        self.retry = None;
        self.loader.stop();
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.kind != StatusIndicatorKind::Compaction {
            return self.loader.render(width);
        }
        let mut status = self.loader.render(width).join(" ").trim_end().to_owned();
        if visible_width(&status) > width {
            self.loader.set_message(&self.compact_label, 0);
            status = self.loader.render(width).join(" ").trim_end().into();
        }
        let budget = width
            .saturating_sub(visible_width(&status) + 1)
            .min(visible_width(&self.progress));
        if budget > 0 {
            let tail = slice_by_column(
                &self.progress,
                visible_width(&self.progress) - budget,
                budget,
                false,
            );
            status.push(' ');
            status.push_str(&self.theme.fg(ThemeColor::Muted, &tail));
        }
        vec![truncate_to_width(&status, width, "...", false)]
    }
}
pub struct IdleStatus {
    pub height: usize,
}
impl Default for IdleStatus {
    fn default() -> Self {
        Self { height: 2 }
    }
}
impl Component for IdleStatus {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![" ".repeat(width); self.height]
    }
}
