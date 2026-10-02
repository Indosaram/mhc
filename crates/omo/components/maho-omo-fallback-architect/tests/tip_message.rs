use maho_omo_fallback_architect::tip_message::*;

#[test]
fn persisted_tip_type_is_stable() {
    assert_eq!(FALLBACK_ARCHITECT_TIP_TYPE, "omo-fallback-architect:tip");
}

#[test]
fn renderer_normalizes_each_line_and_applies_dim_style() {
    let lines = render_fallback_tip("first\r\nsecond\tline", |text| format!("\x1b[2m{text}\x1b[0m"));
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line.starts_with("\x1b[2m") && line.ends_with("\x1b[0m")));
    assert!(!lines.iter().any(|line| line.contains('\r') || line.contains('\t')));
    assert_eq!(senpi_task::renderer_text::normalize_renderer_text(&lines[0]), "Tip: first");
    assert_eq!(senpi_task::renderer_text::normalize_renderer_text(&lines[1]), "second line");
}
