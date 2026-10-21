use crate::{format::{pad_visible, sanitize_terminal_text, truncate_visible}, types::{PanelColor, PanelRow, Theme}};
use super::{viewport::{clamp_scroll, popup_budget}, wheel::{parse_panel_wheel_event, WHEEL_ROWS}};

pub struct TextPopup { pub title: String, pub scroll: usize, pub ratio: f64 }

impl TextPopup {
    pub fn new(title: String) -> Self { Self { title, scroll: 0, ratio: 0.72 } }

    pub fn render(&mut self, width: usize, terminal_rows: usize, rows: &[PanelRow], theme: Option<&Theme>) -> Vec<String> {
        let budget = popup_budget(terminal_rows, self.ratio);
        let inner = width.saturating_sub(4);
        self.scroll = clamp_scroll(self.scroll, rows.len(), budget.body);
        let paint = |color: PanelColor, text: &str| theme.map_or_else(||text.to_owned(), |theme|theme(color, text));
        let frame = |text: &str| format!("{} {} {}",paint(PanelColor::Dim,"│"),text,paint(PanelColor::Dim,"│"));
        let border = "─".repeat(width.saturating_sub(2));
        let mut output = vec![paint(PanelColor::Dim, &format!("┌{border}┐"))];
        let title = truncate_visible(&sanitize_terminal_text(&self.title), inner);
        output.push(frame(&pad_visible(&paint(PanelColor::Accent,&title),inner)));
        for row in rows.iter().skip(self.scroll).take(budget.body) {
            let text = truncate_visible(&sanitize_terminal_text(&row.text), inner);
            let text = row.color.map_or_else(||text.clone(),|color|paint(color,&text));
            output.push(frame(&pad_visible(&text,inner)));
        }
        let hint = if rows.len()>budget.body {
            format!("{}-{}/{}  ↑↓/wheel  esc close",self.scroll+1,(self.scroll+budget.body).min(rows.len()),rows.len())
        } else { "esc close".into() };
        output.push(frame(&pad_visible(&paint(PanelColor::Dim,&truncate_visible(&hint,inner)),inner)));
        output.push(paint(PanelColor::Dim,&format!("└{border}┘")));
        output.truncate(budget.total);
        output
    }

    pub fn handle_input(&mut self, data: &str) -> PopupInput {
        let kitty_escape = data.strip_prefix("\x1b[27").and_then(|rest|rest.strip_suffix('u'))
            .is_some_and(|rest|rest.is_empty() || rest.starts_with(';') && rest.chars().all(|c|c==';'||c.is_ascii_digit()));
        if matches!(data,"\x1b"|"q"|"\r"|"\n") || kitty_escape { return PopupInput::Close; }
        let step = match data {
            "\x1b[B"|"j" => 1, "\x1b[A"|"k" => -1,
            "\x1b[6~" => 10, "\x1b[5~" => -10,
            _ => parse_panel_wheel_event(data).unwrap_or(0)*WHEEL_ROWS,
        };
        if step==0 { return PopupInput::Ignored; }
        let amount=usize::try_from(step.unsigned_abs()).unwrap_or(usize::MAX);
        self.scroll=if step<0 {self.scroll.saturating_sub(amount)} else {self.scroll.saturating_add(amount)};
        PopupInput::Repaint
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupInput { Close, Repaint, Ignored }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scrolling_and_resize_keep_closing_border_and_bound_offset() {
        let rows:Vec<_>=(0..40).map(|n|PanelRow::new(format!("row {n}"))).collect();
        let mut popup=TextPopup::new("file".into());
        assert_eq!(popup.handle_input("\x1b[6~"),PopupInput::Repaint);
        assert_eq!(popup.scroll,10);
        assert_eq!(popup.handle_input("\x1b[<65;1;1M"),PopupInput::Repaint);
        assert_eq!(popup.scroll,13);
        let rendered=popup.render(60,24,&rows,None);
        assert_eq!(rendered.len(),17);
        assert!(rendered.last().expect("bottom border").ends_with('┘'));
        popup.scroll=usize::MAX;
        popup.render(60,24,&rows,None);
        assert_eq!(popup.scroll,27);
        assert_eq!(popup.handle_input("\x1b[27;1u"),PopupInput::Close);
        assert_eq!(popup.handle_input("x"),PopupInput::Ignored);
    }
}
