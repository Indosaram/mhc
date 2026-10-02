use maho_ext_api::{Component, Theme};
use super::spec::{NoticeSpec, NoticeTone};

struct NoticeBox { lines: Vec<String>, background: String }
impl Component for NoticeBox {
    fn render(&mut self, width: usize) -> Vec<String> {
        let content_width = width.saturating_sub(2).max(1);
        let background = &self.background;
        let apply = |text: &str| {
            maho_ext_api::notice_background(text, width, background)
        };
        let mut result = vec![apply("")];
        for line in &self.lines {
            for wrapped in maho_ext_api::notice_wrap_text(line, content_width) {
                result.push(apply(&format!(" {wrapped}")));
            }
        }
        result.push(apply(""));
        result
    }
    fn invalidate(&mut self) {}
}

pub fn build_notice_box(spec: NoticeSpec, expanded: bool, theme: &Theme) -> Box<dyn Component> {
    let style = |tone: NoticeTone, text: &str| {
        let ansi = theme.colors.get(tone.as_str()).map_or("\x1b[39m", String::as_str);
        format!("{ansi}{text}\x1b[39m")
    };
    let mut lines = vec![style(spec.tone.unwrap_or(NoticeTone::Accent), &format!("\x1b[1m{}\x1b[22m", spec.title)), style(NoticeTone::Dim, &spec.why)];
    lines.extend(spec.extra.iter().map(|line| style(line.tone.unwrap_or(NoticeTone::Dim), &line.text)));
    if expanded && let Some(line) = spec.expanded_line { lines.push(style(NoticeTone::Dim, &line)); }
    Box::new(NoticeBox { lines, background: theme.backgrounds.get("customMessageBg").cloned().unwrap_or_else(|| "\x1b[49m".into()) })
}
