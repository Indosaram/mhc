use maho_ext_api::*;
use maho_ext_host::notice::*;

fn spec() -> NoticeSpec {
    NoticeSpec { title: "Title".into(), tone: None, why: "Why".into(), extra: vec![NoticeLine { text: "Extra".into(), tone: Some(NoticeTone::Warning) }], expanded_line: Some("Detail".into()) }
}

#[test]
fn collapsed_notice_omits_expanded_detail_and_preserves_padding() {
    let mut component = build_notice_box(spec(), false, &Theme::default());
    let lines = component.render(12);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], "\x1b[49m            \x1b[49m");
    assert!(lines[1].contains("\x1b[1mTitle\x1b[22m"));
    assert!(lines[3].contains("Extra"));
}

#[test]
fn expanded_notice_includes_detail_and_wraps_to_content_width() {
    let mut component = build_notice_box(spec(), true, &Theme::default());
    let lines = component.render(5);
    assert!(lines.iter().any(|line| line.contains("Det")));
    assert!(lines.iter().any(|line| line.contains("ail")));
}

#[test]
fn notice_message_mapper_can_decline_rendering() {
    let renderer = notice_message_renderer(|_| None);
    let message = CustomMessage { custom_type: "notice".into(), content: Vec::new(), display: true, details: None };
    assert!(renderer(&message, &MessageRenderOptions::default(), &Theme::default()).is_none());
}

#[test]
fn notice_entry_mapper_renders_durable_entry() {
    let renderer = notice_entry_renderer(|entry| (entry.kind == "custom").then(spec));
    let entry = SessionEntry { id: "entry".into(), parent_id: None, timestamp: "now".into(), kind: "custom".into(), data: JsonValue::Null };
    let mut component = renderer(&entry, &EntryRenderOptions { expanded: true }, &Theme::default()).unwrap();
    assert!(component.render(20).iter().any(|line| line.contains("Detail")));
}

#[test]
fn notice_bytes_match_pinned_upstream_colored_box() {
    let theme = Theme {
        colors: [("accent".into(), "\x1b[31m".into()), ("dim".into(), "\x1b[31m".into()), ("warning".into(), "\x1b[31m".into())].into_iter().collect(),
        backgrounds: [("customMessageBg".into(), "\x1b[44m".into())].into_iter().collect(), ..Default::default()
    };
    let mut component = build_notice_box(spec(), true, &theme);
    assert_eq!(component.render(12), [
        "\x1b[44m            \x1b[49m",
        "\x1b[44m \x1b[31m\x1b[1mTitle\x1b[22m\x1b[39m      \x1b[49m",
        "\x1b[44m \x1b[31mWhy\x1b[39m        \x1b[49m",
        "\x1b[44m \x1b[31mExtra\x1b[39m      \x1b[49m",
        "\x1b[44m \x1b[31mDetail\x1b[39m     \x1b[49m",
        "\x1b[44m            \x1b[49m",
    ]);
}
