use std::sync::Arc;
use maho_tui::tui::Component;
use maho_ext_rules::ui::dynamic_border::DynamicBorder;

#[test]
fn border_has_one_line_and_minimum_one_cell() {
    let mut border = DynamicBorder::new(Arc::new(|text| format!("\x1b[31m{text}\x1b[39m")));
    assert_eq!(border.render(0), border.render(1));
    assert_eq!(border.render(4), ["\x1b[31m────\x1b[39m"]);
    border.invalidate();
    assert_eq!(border.render(4).len(), 1);
}
