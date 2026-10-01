//! Port of senpi `packages/coding-agent/src/modes/interactive/components/earendil-announcement.ts`.
//!
//! `buildNoticeBox` lives in `core/extensions/notice/` (not part of this crate's source root), so
//! this module renders the same box privately: a `Box(1,1)` on `customMessageBg` with the title in
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

fn wrap_text(text: String) -> Rc<RefCell<dyn Component>> {
    struct WrappedText {
        text: String,
    }
    impl Component for WrappedText {
        fn render(&mut self, width: usize) -> Vec<String> {
            wrap_text_with_ansi(&self.text, width.max(1))
        }
        fn invalidate(&mut self) {}
    }
    Rc::new(RefCell::new(WrappedText { text }))
}

fn notice_box(theme: &Theme, title: &str, why: &str, extra: &[(ThemeColor, String)]) -> Rc<RefCell<dyn Component>> {
    let theme_for_bg = theme.clone();
    let mut box_component = Box::with_padding(1, 1);
    let background: BackgroundFn = Rc::new(move |text: &str| theme_for_bg.bg(ThemeBg::CustomMessageBg, text));
    box_component.set_bg_fn(Some(background));
    box_component.add_child(wrap_text(theme.fg(
        ThemeColor::Accent,
        &format!("\x1b[1m{title}\x1b[22m"),
    )));
    box_component.add_child(wrap_text(theme.fg(ThemeColor::Dim, why)));
    for (tone, text) in extra {
        box_component.add_child(wrap_text(theme.fg(*tone, text)));
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
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub struct EarendilAnnouncementComponent {
    root: Container,
}

impl EarendilAnnouncementComponent {
    pub fn new(theme: &Theme) -> Self {
        let mut root = Container::new();
        root.add_child(notice_box(
            theme,
            "pi has joined Earendil",
            "Read the blog post:",
            &[(ThemeColor::Accent, BLOG_URL.to_string())],
        ));
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
