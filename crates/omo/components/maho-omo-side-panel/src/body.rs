use std::sync::Arc;
use crate::{format::{sanitize_terminal_text, truncate_visible, pad_visible}, links::with_action_link, types::{PanelComponent, RowSource, Theme}};
pub struct PanelBody { source: RowSource, theme: Arc<dyn Fn() -> Option<Theme> + Send + Sync>, clickable: Arc<dyn Fn() -> bool + Send + Sync> }
pub fn create_panel_body(source: RowSource, theme: Arc<dyn Fn() -> Option<Theme> + Send + Sync>, clickable: Arc<dyn Fn() -> bool + Send + Sync>) -> PanelBody { PanelBody { source, theme, clickable } }
impl PanelComponent for PanelBody {
    fn render(&self, width: usize) -> Vec<String> {
        let paint = (self.theme)(); let links = (self.clickable)();
        (self.source)(width).into_iter().map(|row| {
            let text = truncate_visible(&sanitize_terminal_text(&row.text), width);
            let styled = match (&paint, row.color) { (Some(theme), Some(color)) => theme(color, &text), (None, _) | (Some(_), None) => text };
            let padded = pad_visible(&styled, width);
            match (links, row.action) { (true, Some(action)) => with_action_link(&padded, &action), (false, _) | (true, None) => padded }
        }).collect()
    }
}
#[cfg(test)] mod tests {
    use super::*; use crate::{links::PanelAction, types::{PanelRow, PanelColor}};
    fn body(rows: Vec<PanelRow>, themed: bool, clickable: bool) -> PanelBody { create_panel_body(Arc::new(move |_| rows.clone()), Arc::new(move || themed.then(|| Arc::new(|_: PanelColor, text: &str| format!("\x1b[35m{text}\x1b[39m")) as Theme)), Arc::new(move || clickable)) }
    #[test] fn padding_truncation_and_plain_fallback() {
        assert_eq!(body(vec![PanelRow::new("a"),PanelRow::new("bb")],false,false).render(5), ["a    ","bb   "]);
        assert_eq!(body(vec![PanelRow::new("abcdefghij")],false,false).render(5), ["abcd…"]);
        assert_eq!(body(vec![PanelRow::colored("USAGE",PanelColor::Accent)],false,false).render(5), ["USAGE"]);
        assert_eq!(body(vec![PanelRow::new("plain")],true,false).render(6), ["plain "]);
        assert_eq!(body(vec![],false,false).render(0), Vec::<String>::new());
    }
    #[test] fn theme_runs_after_sanitizing_and_before_padding() {
        assert_eq!(body(vec![PanelRow::colored("USAGE",PanelColor::Accent)],true,false).render(8), ["\x1b[35mUSAGE\x1b[39m   "]);
        assert_eq!(body(vec![PanelRow::colored("a\x1b]8;;https://x\x07b\nc\x1b[2J",PanelColor::Accent)],true,false).render(22), ["\x1b[35ma]8;;https://xb c[2J\x1b[39m  "]);
    }
    #[test] fn hyperlink_wraps_padding_only_when_enabled() {
        let row = PanelRow { text: "M a.ts".into(), color: None, action: Some(PanelAction::File { path: "src/a.ts".into() }) };
        assert_eq!(body(vec![row.clone()],false,true).render(10), ["\x1b]8;;omo-panel:file/src%2Fa.ts\x07M a.ts    \x1b]8;;\x07"]);
        assert_eq!(body(vec![row],false,false).render(10), ["M a.ts    "]);
        assert_eq!(body(vec![PanelRow::new("FILES")],false,true).render(10), ["FILES     "]);
    }
}
