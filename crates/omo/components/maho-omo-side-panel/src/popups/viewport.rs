pub const POPUP_CHROME_ROWS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupBudget { pub total: usize, pub body: usize }

pub fn popup_budget(terminal_rows: usize, ratio: f64) -> PopupBudget {
    let rows = if terminal_rows == 0 { 24 } else { terminal_rows };
    let share = if ratio.is_finite() && ratio > 0.0 { ratio.min(1.0) } else { 0.72 };
    let fraction = f64::from(u32::try_from(rows).unwrap_or(u32::MAX)) * share;
    let allocated = fraction.floor() as usize;
    let total = rows.min(allocated.max(POPUP_CHROME_ROWS + 1));
    PopupBudget { total, body: total.saturating_sub(POPUP_CHROME_ROWS) }
}

pub fn clamp_scroll(scroll: usize, content_rows: usize, body_rows: usize) -> usize {
    scroll.min(content_rows.saturating_sub(body_rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_keeps_chrome_inside_short_and_default_terminals() {
        assert_eq!(popup_budget(24, 0.72), PopupBudget { total: 17, body: 13 });
        assert_eq!(popup_budget(3, 0.72), PopupBudget { total: 3, body: 0 });
        assert_eq!(popup_budget(0, f64::NAN), popup_budget(24, 0.72));
        assert_eq!(clamp_scroll(100, 20, 13), 7);
    }
}
