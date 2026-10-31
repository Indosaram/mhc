use maho_omo_fallback_architect::tip_message::*;

#[test]
fn persisted_tip_type_is_stable() {
    assert_eq!(FALLBACK_ARCHITECT_TIP_TYPE, "omo-fallback-architect:tip");
}

#[test]
fn renderer_normalizes_each_line_and_applies_dim_style() {
    let lines = render_fallback_tip("first\r\nsecond\tline", |text| {
        format!("\x1b[2m{text}\x1b[0m")
    });
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|line| line.starts_with("\x1b[2m") && line.ends_with("\x1b[0m"))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains('\r') || line.contains('\t'))
    );
    assert_eq!(
        senpi_task::renderer_text::normalize_renderer_text(&lines[0]),
        "Tip: first"
    );
    assert_eq!(
        senpi_task::renderer_text::normalize_renderer_text(&lines[1]),
        "second line"
    );
}

// tip-message.test.ts: the persisted tip renders with a Tip: label, every line dimmed through the theme, content intact.
#[test]
fn persisted_tip_renders_label_dim_theme_and_content() {
    let mut theme = maho_ext_api::Theme::default();
    theme.colors.insert("dim".into(), "\x1b[2m".into());
    let text = build_fallback_tip_text("anthropic/claude-fable-5", "anthropic/claude-opus-5");
    let rendered = render_fallback_tip_with_theme(&text, &theme);
    assert_eq!(rendered.len(), text.split('\n').count());
    assert!(rendered[0].contains("Tip:"));
    assert!(
        rendered
            .iter()
            .all(|line| line.starts_with("\x1b[2m") && line.ends_with("\x1b[39m"))
    );
    let joined = rendered.join("\n");
    assert!(joined.contains("anthropic/claude-opus-5"));
    assert!(joined.contains("anthropic/claude-fable-5"));
}
