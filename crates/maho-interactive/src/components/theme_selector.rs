//! Port of components/theme-selector.ts.
//!
//! Shared selector chrome lives here: senpi's `DynamicBorder` (todo 32 owns its standalone module)
//! and `getSelectListTheme` (todo 31's theme module keeps the palette, not the widget theme) are
//! reproduced as small helpers so the selector family renders byte-identically to pinned senpi.

use std::rc::Rc;

use maho_tui::components::select_list::{
    OnCancel, OnSelect, SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use maho_tui::tui::Component;

use crate::theme::theme::{Theme, ThemeColor};

/// senpi's `DynamicBorder`: one horizontal rule of the viewport width.
pub(crate) fn dynamic_border(theme: &Theme, color: ThemeColor, width: usize) -> String {
    theme.fg(color, &"─".repeat(width.max(1)))
}

/// senpi's `getSelectListTheme()`.
pub(crate) fn select_list_theme(theme: &Theme) -> SelectListTheme {
    let accent = theme.clone();
    let muted = theme.clone();
    SelectListTheme {
        selected_prefix: Rc::new(move |text: &str| accent.fg(ThemeColor::Accent, text)),
        selected_text: {
            let accent = theme.clone();
            Rc::new(move |text: &str| accent.fg(ThemeColor::Accent, text))
        },
        description: {
            let muted = muted.clone();
            Rc::new(move |text: &str| muted.fg(ThemeColor::Muted, text))
        },
        scroll_info: {
            let muted = muted.clone();
            Rc::new(move |text: &str| muted.fg(ThemeColor::Muted, text))
        },
        no_match: Rc::new(move |text: &str| muted.fg(ThemeColor::Muted, text)),
        render_row: None,
    }
}

pub(crate) fn theme_select_list_layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12),
        max_primary_column_width: Some(32),
        truncate_primary: None,
    }
}

/// Component that renders a theme selector.
pub struct ThemeSelectorComponent {
    theme: Theme,
    select_list: SelectList,
}

impl ThemeSelectorComponent {
    pub fn new(
        theme: &Theme,
        current_theme: &str,
        available_themes: &[String],
        on_select: Box<dyn FnMut(&str)>,
        on_cancel: Box<dyn FnMut()>,
        on_preview: Box<dyn FnMut(&str)>,
    ) -> Self {
        let items: Vec<SelectItem> = available_themes
            .iter()
            .map(|name| SelectItem {
                value: name.clone(),
                label: name.clone(),
                description: (name == current_theme).then(|| "(current)".to_owned()),
            })
            .collect();
        let mut select_list =
            SelectList::new(items, 10, select_list_theme(theme), theme_select_list_layout());
        if let Some(index) = available_themes.iter().position(|name| name == current_theme) {
            select_list.set_selected_index(index);
        }
        let mut on_select = on_select;
        let mut on_preview = on_preview;
        select_list.on_select = Some(Box::new(move |item: &SelectItem| on_select(&item.value)) as OnSelect);
        select_list.on_cancel = Some(on_cancel as OnCancel);
        select_list.on_selection_change =
            Some(Box::new(move |item: &SelectItem| on_preview(&item.value)) as OnSelect);
        Self {
            theme: theme.clone(),
            select_list,
        }
    }

    pub fn select_list(&self) -> &SelectList {
        &self.select_list
    }

    pub fn select_list_mut(&mut self) -> &mut SelectList {
        &mut self.select_list
    }
}

impl Component for ThemeSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width)];
        lines.extend(self.select_list.render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
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
