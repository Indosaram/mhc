use crate::{constants::{MIN_TRANSCRIPT_COLUMNS, PANEL_MIN_COLUMNS, PANEL_MAX_COLUMNS}, settings::PanelWidth};
pub fn resolve_panel_width(configured: &PanelWidth, terminal_width: usize) -> usize {
    let requested = match configured { PanelWidth::Columns(n) => *n, PanelWidth::Percent(s) => s.strip_suffix('%').and_then(|n| n.parse::<usize>().ok()).filter(|_|terminal_width > 0).map_or(PANEL_MIN_COLUMNS, |n|terminal_width.saturating_mul(n)/100) };
    let clamped = requested.clamp(PANEL_MIN_COLUMNS,PANEL_MAX_COLUMNS);
    if terminal_width == 0 { return clamped; }
    let affordable = terminal_width.saturating_sub(MIN_TRANSCRIPT_COLUMNS);
    if affordable < PANEL_MIN_COLUMNS { PANEL_MIN_COLUMNS } else { clamped.min(affordable) }
}
