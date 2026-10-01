//! Port of senpi `packages/coding-agent/src/modes/interactive/components/earendil-announcement.ts`.
//!
//! `buildNoticeBox` lives in `core/extensions/notice/` (outside this crate's source root), so the
//! notice box is rendered here directly: a `Box(1, 1)` on `customMessageBg` carrying the title in
//! the accent tone, the why line dim, and the extra lines in their own tone.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::box_::{BackgroundFn, Box};
use maho_tui::components::image::{Image, ImageOptions, ImageTheme};
use maho_tui::components::spacer::Spacer;
use maho_tui::tui::{Component, Container};
use maho_tui::utils::wrap_text_with_ansi;

use crate::theme::{Theme, ThemeBg, ThemeColor};

const BLOG_URL: &str = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";
const IMAGE_FILENAME: &str = "clankolas.png";
const BOLD: &str = "\x1b[1m";
const BOLD_OFF: &str = "\x1b[22m";

struct NoticeText {
    text: String,
}

impl Component for NoticeText {
    fn render(&mut self, width: usize) -> Vec<String> {
        wrap_text_with_ansi(&self.text, width.max(1))
    }

    fn invalidate(&mut self) {}
}

fn notice_text(text: String) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(NoticeText { text }))
}

pub struct NoticeLine {
    pub text: String,
    pub tone: Option<ThemeColor>,
}

pub struct NoticeSpec {
    pub title: String,
    pub tone: Option<ThemeColor>,
    pub why: String,
    pub extra: Vec<NoticeLine>,
    pub expanded_line: Option<String>,
}

pub fn build_notice_box(spec: &NoticeSpec, expanded: bool, theme: &Theme) -> Rc<RefCell<dyn Component>> {
    let theme_for_bg = theme.clone();
    let background: BackgroundFn = Rc::new(move |text: &str| theme_for_bg.bg(ThemeBg::CustomMessageBg, text));
    let mut box_component = Box::with_padding(1, 1);
    box_component.set_bg_fn(Some(background));
    box_component.add_child(notice_text(theme.fg(
        spec.tone.unwrap_or(ThemeColor::Accent),
        &format!("{BOLD}{}{BOLD_OFF}", spec.title),
    )));
    box_component.add_child(notice_text(theme.fg(ThemeColor::Dim, &spec.why)));
    for line in &spec.extra {
        box_component.add_child(notice_text(theme.fg(line.tone.unwrap_or(ThemeColor::Dim), &line.text)));
    }
    if expanded
        && let Some(expanded_line) = &spec.expanded_line
    {
        box_component.add_child(notice_text(theme.fg(ThemeColor::Dim, expanded_line)));
    }
    Rc::new(RefCell::new(box_component))
}

fn load_image_base64() -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join(IMAGE_FILENAME);
    let bytes = std::fs::read(path).ok()?;
    Some(base64_encode(&bytes))
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub struct EarendilAnnouncementComponent {
    root: Container,
}

impl EarendilAnnouncementComponent {
    pub fn new(theme: &Theme) -> Self {
        let mut root = Container::new();
        let spec = NoticeSpec {
            title: "pi has joined Earendil".to_string(),
            tone: Some(ThemeColor::Accent),
            why: "Read the blog post:".to_string(),
            extra: vec![NoticeLine {
                text: BLOG_URL.to_string(),
                tone: Some(ThemeColor::Accent),
            }],
            expanded_line: None,
        };
        root.add_child(build_notice_box(&spec, false, theme));
        root.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        if let Some(base64) = load_image_base64() {
            let theme_for_fallback = theme.clone();
            let image = Image::new(
                base64,
                "image/png",
                ImageTheme {
                    fallback_color: Rc::new(move |text: &str| theme_for_fallback.fg(ThemeColor::Muted, text)),
                },
                ImageOptions {
                    max_width_cells: Some(56),
                    filename: Some(IMAGE_FILENAME.to_string()),
                    ..Default::default()
                },
                None,
            );
            root.add_child(Rc::new(RefCell::new(image)));
            root.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        }

        Self { root }
    }
}

impl Component for EarendilAnnouncementComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.root.render(width)
    }

    fn invalidate(&mut self) {
        self.root.invalidate();
    }
}
