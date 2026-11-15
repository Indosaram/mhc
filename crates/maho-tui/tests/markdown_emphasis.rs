//! senpi `MarkdownTheme.strong`/`emphasis`/`deleted` (PR #3195): the optional hooks that let a theme
//! color inline `**strong**`, `*emphasis*` and `~~deleted~~` text. Omitting a hook keeps the plain
//! bold/italic/strikethrough path, so themes that do not set them render exactly as before.

use maho_tui::components::markdown::{
    DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme, ThemeFn,
};

fn wrap(open: &'static str, close: &'static str) -> ThemeFn {
    std::sync::Arc::new(move |text: &str| format!("\u{1b}[{open}m{text}\u{1b}[{close}m"))
}

fn theme() -> MarkdownTheme {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(20_000);
    MarkdownTheme {
        id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        heading: wrap("1;36", "0"),
        link: wrap("34", "39"),
        link_url: wrap("2", "22"),
        code: wrap("33", "39"),
        code_block: wrap("32", "39"),
        code_block_border: wrap("2", "22"),
        quote: wrap("3", "23"),
        quote_border: wrap("2", "22"),
        hr: wrap("2", "22"),
        list_bullet: wrap("36", "39"),
        bold: wrap("1", "22"),
        italic: wrap("3", "23"),
        strikethrough: wrap("9", "29"),
        underline: wrap("4", "24"),
        strong: None,
        emphasis: None,
        deleted: None,
        highlight_code: None,
        code_block_indent: None,
    }
}

fn render(
    text: &str,
    theme: MarkdownTheme,
    default_text_style: Option<DefaultTextStyle>,
) -> String {
    let mut markdown = Markdown::new(
        text,
        0,
        0,
        theme,
        default_text_style,
        MarkdownOptions::default(),
    );
    markdown.render(200).join("\n")
}

/// The escape that opens the styled run for `word`, if the run starts with `open` directly.
fn opens_with(output: &str, word: &str, open: &str) -> bool {
    output
        .find(word)
        .is_some_and(|index| output[..index].ends_with(open))
}

#[test]
fn omitting_the_hooks_keeps_the_bold_italic_strikethrough_path() {
    let output = render("**bold** *italic* ~~gone~~", theme(), None);
    assert!(opens_with(&output, "bold", "\u{1b}[1m"), "{output:?}");
    assert!(opens_with(&output, "italic", "\u{1b}[3m"), "{output:?}");
    assert!(opens_with(&output, "gone", "\u{1b}[9m"), "{output:?}");
    for color in ["\u{1b}[35m", "\u{1b}[36m", "\u{1b}[31m"] {
        assert!(
            !output.contains(color),
            "no hook color without a hook: {output:?}"
        );
    }
}

#[test]
fn the_hooks_replace_bold_italic_strikethrough_for_strong_emphasis_and_deleted() {
    let mut themed = theme();
    themed.strong = Some(wrap("35", "39"));
    themed.emphasis = Some(wrap("36", "39"));
    themed.deleted = Some(wrap("31", "39"));
    let output = render("**bold** *italic* ~~gone~~", themed, None);
    assert!(opens_with(&output, "bold", "\u{1b}[35m"), "{output:?}");
    assert!(opens_with(&output, "italic", "\u{1b}[36m"), "{output:?}");
    assert!(opens_with(&output, "gone", "\u{1b}[31m"), "{output:?}");
    // The plain bold/italic/strikethrough escapes are gone: the hook owns the run's style.
    for plain in ["\u{1b}[1mbold", "\u{1b}[3mitalic", "\u{1b}[9mgone"] {
        assert!(
            !output.contains(plain),
            "plain style still applied: {output:?}"
        );
    }
}

#[test]
fn each_hook_only_affects_its_own_token() {
    let mut themed = theme();
    themed.deleted = Some(wrap("31", "39"));
    let output = render("**bold** *italic* ~~gone~~", themed, None);
    assert!(opens_with(&output, "bold", "\u{1b}[1m"), "{output:?}");
    assert!(opens_with(&output, "italic", "\u{1b}[3m"), "{output:?}");
    assert!(opens_with(&output, "gone", "\u{1b}[31m"), "{output:?}");
}

#[test]
fn nested_inline_styles_still_render_inside_a_hook() {
    let mut themed = theme();
    themed.strong = Some(wrap("35", "39"));
    let output = render("**lead *inner* tail**", themed, None);
    let lead = output.find("lead").expect("lead survives");
    assert!(
        output[..lead].ends_with("\u{1b}[35m"),
        "hook opens the run: {output:?}"
    );
    let inner = output.rfind("inner").expect("inner survives");
    let italic = output[..inner]
        .rfind("\u{1b}[3m")
        .expect("nested emphasis stays italic");
    assert!(
        italic > lead,
        "the nested italic opens inside the hooked run, not before it: {output:?}"
    );
    let tail = output.find("tail").expect("tail survives");
    assert!(lead < inner && inner < tail, "content order: {output:?}");
}

#[test]
fn a_hook_color_is_applied_inside_the_surrounding_text_color() {
    let mut themed = theme();
    themed.strong = Some(wrap("35", "39"));
    let style = DefaultTextStyle {
        color: Some(wrap("31", "39")),
        ..DefaultTextStyle::default()
    };
    let output = render("**hi**", themed, Some(style));
    let text = output.find("hi").expect("text survives");
    let closest = |code: &str| output[..text].rfind(code);
    let surrounding = closest("\u{1b}[31m").expect("surrounding color");
    let hook = closest("\u{1b}[35m").expect("hook color");
    assert!(
        hook > surrounding,
        "hook color is emitted inside the surrounding color so it wins: {output:?}"
    );
}
