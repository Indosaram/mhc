use maho_ext_pi_rules::ui::dynamic_border::DynamicBorder;
use maho_tui::tui::Component;
#[test]fn border_calls_color_with_width_and_no_cached_output(){let mut border=DynamicBorder::new(|text:&str|format!("[{text}]"));assert_eq!(border.render(0),vec!["[\u{2500}]"]);assert_eq!(border.render(3),vec!["[\u{2500}\u{2500}\u{2500}]"]);border.invalidate();assert_eq!(border.render(1),vec!["[\u{2500}]"]);}
