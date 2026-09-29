//! Port of senpi `packages/tui/src/terminal-text.ts`.

use std::sync::LazyLock;

use regex::Regex;

/// Image pixel dimensions (`ImageDimensions` from terminal-image.ts, todo 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

static TERMINAL_ESCAPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:\x{1B}\][\s\S]*?(?:\x{07}|\x{1B}\\|\x{9C}))|[\x{1B}\x{9B}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]",
    )
    .expect("valid regex")
});
static CONTROL_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x{00}-\x{1f}\x{7f}-\x{9f}]+").expect("valid regex"));
static JS_WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\t\n\x{0B}\x{0C}\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]+")
        .expect("valid regex")
});

/// Trims the JS `String.prototype.trim` whitespace set.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| JS_WHITESPACE_RUN.is_match(c.encode_utf8(&mut [0; 4])))
}

pub fn sanitize_terminal_label(value: &str) -> String {
    let stripped = TERMINAL_ESCAPE_PATTERN.replace_all(value, "");
    let spaced = CONTROL_RUN.replace_all(&stripped, " ");
    let collapsed = JS_WHITESPACE_RUN.replace_all(&spaced, " ");
    js_trim(&collapsed).to_string()
}

pub fn shorten_image_path(filename: &str) -> String {
    let home = dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    shorten_image_path_with_home(filename, &home)
}

fn shorten_image_path_with_home(filename: &str, home: &str) -> String {
    if !home.is_empty()
        && (filename == home
            || filename.starts_with(&format!("{home}/"))
            || filename.starts_with(&format!("{home}\\")))
    {
        return format!("~{}", &filename[home.len()..]);
    }
    filename.to_string()
}

pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(filename) = filename.filter(|f| !f.is_empty()) {
        parts.push(shorten_image_path(&sanitize_terminal_label(filename)));
    }
    parts.push(format!("[{}]", sanitize_terminal_label(mime_type)));
    if let Some(d) = dimensions {
        parts.push(format!("{}x{}", d.width_px, d.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_safe_image_label(label: &str) {
        assert!(
            !label
                .chars()
                .any(|c| matches!(u32::from(c), 0..=0x1f | 0x7f..=0x9f)),
            "{label:?}"
        );
        assert!(!label.contains('\x1b'));
        assert!(!label.contains('\x07'));
        assert!(!label.contains("\x1b]"));
    }

    #[test]
    fn given_normal_image_metadata_when_rendered_then_the_existing_label_format_is_preserved() {
        let dimensions = ImageDimensions {
            width_px: 640,
            height_px: 480,
        };
        let label = image_fallback("image/png", Some(dimensions), Some("preview.png"));
        assert_eq!(label, "[Image: preview.png [image/png] 640x480]");
    }

    #[test]
    fn given_hostile_mime_and_filename_labels_when_rendered_then_terminal_controls_are_inert() {
        let hostile_mime_type = "image/png\x1b]52;c;SGVsbG8=\x07";
        let hostile_filename = "preview\x1b]0;owned\x07\u{009b}31m\x00.png";
        let label = image_fallback(
            hostile_mime_type,
            Some(ImageDimensions {
                width_px: 1,
                height_px: 1,
            }),
            Some(hostile_filename),
        );
        assert_safe_image_label(&label);
        assert!(!label.contains("SGVsbG8="));
        assert!(!label.contains("owned"));
    }

    #[test]
    fn shortens_paths_under_home() {
        assert_eq!(
            shorten_image_path_with_home("/home/u/a.png", "/home/u"),
            "~/a.png"
        );
        assert_eq!(shorten_image_path_with_home("/home/u", "/home/u"), "~");
        assert_eq!(
            shorten_image_path_with_home("/home/user2/a.png", "/home/u"),
            "/home/user2/a.png"
        );
    }
}
