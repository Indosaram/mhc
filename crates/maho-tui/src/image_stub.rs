//! The renderer's view of senpi `packages/tui/src/terminal-image.ts`: `tui.rs`, `layout.rs` and
//! `tui_alt_screen.rs` call through this facade, and every function delegates to the full port in
//! [`crate::terminal_image`].

pub use crate::terminal_image::{crop_kitty_image_line, delete_all_kitty_images, delete_all_kitty_placements, delete_kitty_image, is_image_line, KittyImagePlacement};

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
    parse_kitty_image_header(line).map(|h| h.ids).unwrap_or_default()
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
    });
}
