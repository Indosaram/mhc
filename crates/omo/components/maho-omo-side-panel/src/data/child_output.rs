use crate::types::{PanelColor, PanelRow};
pub const NO_OUTPUT_NOTICE: &str = "(no transcript recorded for this task)";

pub fn child_output_rows(text: &str, truncated: bool) -> Vec<PanelRow> {
    let body=if text.is_empty() {NO_OUTPUT_NOTICE} else {text.strip_suffix('\n').unwrap_or(text)};
    let mut rows:Vec<_>=body.split('\n').map(|line| {
        let color=if line.starts_with("error")||line.starts_with("tool[error]") {PanelColor::Error}
            else if line.starts_with("tool") {PanelColor::Muted}
            else if line==NO_OUTPUT_NOTICE {PanelColor::Dim} else {PanelColor::Text};
        PanelRow::colored(line,color)
    }).collect();
    if truncated {rows.push(PanelRow::colored("(earlier output elided by the task engine's transcript cap)",PanelColor::Dim));}
    rows
}
