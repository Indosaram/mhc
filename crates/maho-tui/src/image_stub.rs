//! The renderer's view of senpi `packages/tui/src/terminal-image.ts`: `tui.rs`, `layout.rs` and
//! `tui_alt_screen.rs` call through this facade, and every function delegates to the full port in
//! [`crate::terminal_image`].

pub use crate::terminal_image::{
    KittyImagePlacement, crop_kitty_image_line, delete_all_kitty_images,
    delete_all_kitty_placements, delete_kitty_image, is_image_line,
};

const KITTY_PREFIX: &str = "\x1b_G";

struct KittyImageHeader {
    ids: Vec<u32>,
    rows: usize,
}

fn parse_kitty_image_header(line: &str) -> Option<KittyImageHeader> {
    let sequence_start = line.find(KITTY_PREFIX)?;
    let params_start = sequence_start + KITTY_PREFIX.len();
    let params_end = params_start + line[params_start..].find(';')?;
    let mut ids = Vec::new();
    let mut rows = 1usize;
    for param in line[params_start..params_end].split(',') {
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        let Ok(number) = value.parse::<u64>() else {
            continue;
        };
        if number == 0 || number > 0xffff_ffff {
            continue;
        }
        match key {
            "i" => ids.push(number as u32),
            "r" => rows = number as usize,
            _ => {}
        }
    }
    Some(KittyImageHeader { ids, rows })
}

pub fn extract_kitty_image_ids(line: &str) -> Vec<u32> {
    parse_kitty_image_header(line)
        .map(|h| h.ids)
        .unwrap_or_default()
}

pub fn extract_kitty_image_rows(line: &str) -> usize {
    parse_kitty_image_header(line).map(|h| h.rows).unwrap_or(1)
}

pub fn get_kitty_image_metadata(line: &str) -> Option<usize> {
    crate::terminal_image::get_kitty_image_metadata(line).map(|metadata| metadata.rows as usize)
}

pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    crate::terminal_image::get_kitty_image_placement(line)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images_supported: bool,
    pub images: ImageProtocol,
}

pub fn get_capabilities() -> TerminalCapabilities {
    let capabilities = crate::terminal_image::get_capabilities();
    TerminalCapabilities {
        images_supported: capabilities.images.is_some(),
        images: match capabilities.images {
            Some(crate::terminal_capabilities::ImageProtocol::Kitty) => ImageProtocol::Kitty,
            Some(crate::terminal_capabilities::ImageProtocol::Iterm2) => ImageProtocol::Iterm2,
            None => ImageProtocol::None,
        },
    }
}

pub fn set_capabilities(capabilities: TerminalCapabilities) {
    crate::terminal_image::set_capabilities(crate::terminal_image::TerminalCapabilities {
        images: match capabilities.images {
            ImageProtocol::Kitty => Some(crate::terminal_capabilities::ImageProtocol::Kitty),
            ImageProtocol::Iterm2 => Some(crate::terminal_capabilities::ImageProtocol::Iterm2),
            ImageProtocol::None => None,
        },
        true_color: true,
        hyperlinks: false,
        tmux_passthrough: false,
        kitty_unicode_placeholders: false,
        image_probe: None,
    });
}

#[cfg(test)]
mod isimageline_regression_tests {
    //! Port of senpi `bug-regression-isimageline-startswith-bug.test.ts`.

    use super::is_image_line;

    const READ_TOOL_LINE: &str =
        "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64data...\x07";

    #[test]
    fn old_implementation_would_return_false_causing_crash() {
        let old_is_image_line = |line: &str, prefix: Option<&str>| -> bool {
            prefix.is_some_and(|prefix| line.starts_with(prefix))
        };
        assert!(!old_is_image_line(READ_TOOL_LINE, None));
    }

    #[test]
    fn new_implementation_returns_true_correctly() {
        assert!(is_image_line(READ_TOOL_LINE));
    }

    #[test]
    fn new_implementation_detects_kitty_sequences_in_any_position() {
        let long = format!(
            "Text before \x1b_Ga=T,f=100{} text after",
            "A".repeat(300_000)
        );
        for line in [
            "At start: \x1b_Ga=T,f=100,data...\x1b\\",
            "Prefix \x1b_Ga=T,data...\x1b\\",
            "Suffix text \x1b_Ga=T,data...\x1b\\ suffix",
            "Middle \x1b_Ga=T,data...\x1b\\ more text",
            long.as_str(),
        ] {
            assert!(is_image_line(line), "{:?}", &line[..line.len().min(50)]);
        }
    }

    #[test]
    fn new_implementation_detects_iterm2_sequences_in_any_position() {
        let long = format!(
            "Text before \x1b]1337;File=size=800,600;inline=1:{} text after",
            "B".repeat(300_000)
        );
        for line in [
            "At start: \x1b]1337;File=size=100,100:base64...\x07",
            "Prefix \x1b]1337;File=inline=1:data==\x07",
            "Suffix text \x1b]1337;File=inline=1:data==\x07 suffix",
            "Middle \x1b]1337;File=inline=1:data==\x07 more text",
            long.as_str(),
        ] {
            assert!(is_image_line(line), "{:?}", &line[..line.len().min(50)]);
        }
    }

    #[test]
    fn detects_image_sequences_in_read_tool_output() {
        assert!(is_image_line(
            "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64image...\x07"
        ));
    }

    #[test]
    fn detects_kitty_sequences_from_image_component() {
        assert!(is_image_line(
            "\x1b_Ga=T,f=100,t=f,d=base64data...\x1b\\\x1b_Gm=i=1;\x1b\\"
        ));
    }

    #[test]
    fn handles_ansi_codes_before_image_sequences() {
        for line in [
            "\x1b[31mError\x1b[0m: \x1b]1337;File=inline=1:base64==\x07",
            "\x1b[33mWarning\x1b[0m: \x1b_Ga=T,data...\x1b\\",
            "\x1b[1mBold\x1b[0m \x1b]1337;File=:base64==\x07\x1b[0m",
        ] {
            assert!(is_image_line(line), "{line:?}");
        }
    }

    #[test]
    fn does_not_crash_on_very_long_lines_with_image_sequences() {
        let crash_line = format!(
            "Output: \x1b]1337;File=size=800,600;inline=1:{} end of output",
            "A".repeat(100).repeat(3040)
        );
        assert!(crash_line.len() > 300_000);
        assert!(is_image_line(&crash_line));
    }

    #[test]
    fn handles_lines_exactly_matching_crash_log_dimensions() {
        let target_width = 58_649;
        let (prefix, sequence, suffix) = ("Text", "\x1b_Ga=T,f=100", "End");
        let padding = "A".repeat(target_width - prefix.len() - sequence.len() - suffix.len());
        let line = format!("{prefix}{sequence}{padding}{suffix}");
        assert_eq!(line.len(), 58_649);
        assert!(is_image_line(&line));
    }

    #[test]
    fn does_not_detect_images_in_regular_long_text() {
        assert!(!is_image_line(&"A".repeat(100_000)));
    }

    #[test]
    fn does_not_detect_images_in_lines_with_file_paths() {
        for path in [
            "/path/to/1337/image.jpg",
            "/usr/local/bin/File_converter",
            "~/Documents/1337File_backup.png",
            "./_G_test_file.txt",
        ] {
            assert!(!is_image_line(path), "{path}");
        }
    }
}
