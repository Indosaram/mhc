//! Minimal image-line helpers needed by `tui.rs` before todo 9 ports senpi
//! `terminal-image.ts` in full. Todo 9 replaces this module with the complete port; only the
//! functions actually called by todo 7's renderer live here.

const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

pub fn is_image_line(line: &str) -> bool {
    line.starts_with(KITTY_PREFIX)
        || line.starts_with(ITERM2_PREFIX)
        || line.contains(KITTY_PREFIX)
        || line.contains(ITERM2_PREFIX)
}

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

/// Row count of the Kitty placeholder starting at `line`, if any (senpi's
/// `getKittyImageMetadata`). Todo 9 replaces this with the full metadata struct (image id,
/// pixel dimensions); `layout.rs` only needs the row count to crop a partially scrolled image.
pub fn get_kitty_image_metadata(line: &str) -> Option<usize> {
    if !is_image_line(line) {
        return None;
    }
    Some(extract_kitty_image_rows(line))
}

/// Crop a Kitty image placeholder line to the rows still visible after `hidden_rows` have
/// scrolled off (senpi's `cropKittyImageLine`). A real crop re-encodes the Kitty placeholder's
/// row/column span; todo 9 owns that encoding. Until then this returns the line unmodified,
/// which under-crops (the image paints its full height) rather than corrupting the placeholder
/// escape sequence - documented in parity.d/7.md.
pub fn crop_kitty_image_line(line: &str, _hidden_rows: usize, _visible_rows: usize) -> String {
    line.to_string()
}

pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={image_id}\x1b\\")
}

/// Delete every image the terminal currently holds (senpi's `deleteAllKittyImages`); todo 9's
/// real port tracks transmitted image ids to target only those. This shim has no such
/// registry, so it issues the protocol's delete-all directive (`d=A`) instead of one
/// delete-by-id call per tracked image - a strictly broader delete, never narrower, so it
/// cannot leave a stale image on screen.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A\x1b\\".to_string()
}

/// Delete every placement (but keep transmitted image data resident) so a redraw can
/// re-composite without a full retransmit (senpi's `deleteAllKittyPlacements`, protocol
/// directive `d=A` restricted to placements via `d=P`... falls back to the same delete-all
/// directive as [`delete_all_kitty_images`] here since this shim tracks no placement ids).
pub fn delete_all_kitty_placements() -> String {
    "\x1b_Ga=d,d=A\x1b\\".to_string()
}

/// Placement metadata senpi's real `getKittyImagePlacement` extracts from a placeholder line's
/// escape sequence, needed by the offscreen-image cache eviction policy in
/// `tui_alt_screen.rs`. Todo 9 replaces this with the full parse (pixel dimensions, transfer
/// medium); this shim only extracts what the eviction budget accounting reads.
pub struct KittyImagePlacement {
    pub image_id: u32,
    pub transmission_generation: u64,
    pub transmission_bytes: u64,
    pub estimated_decoded_bytes: u64,
    pub replacement_line: String,
}

/// Extracts placement metadata from a line carrying a Kitty placeholder, or `None` for a
/// plain line. This shim has no access to the real transmission byte-accounting senpi's
/// terminal-image.ts keeps per upload, so `transmission_generation`/`transmission_bytes`/
/// `estimated_decoded_bytes` are always `0`/derived from the line length - documented as a
/// todo-9 gap in parity.d/7.md. `replacement_line` is the line unchanged (no cached
/// placeholder swap to perform without the real transmission registry).
pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    let header = parse_kitty_image_header(line)?;
    let image_id = *header.ids.first()?;
    Some(KittyImagePlacement {
        image_id,
        transmission_generation: 0,
        transmission_bytes: line.len() as u64,
        estimated_decoded_bytes: line.len() as u64,
        replacement_line: line.to_string(),
    })
}

/// Kitty/iTerm2 image protocol negotiated with the terminal (senpi's `ImageProtocol`).
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

/// senpi's `getCapabilities()` caches a real terminal probe; todo 9 ports that probe. Until
/// then this always reports no image support, matching senpi's degrade-to-`undefined` default
/// for a terminal that hasn't answered a capability query.
pub fn get_capabilities() -> TerminalCapabilities {
    TerminalCapabilities {
        images_supported: false,
        images: ImageProtocol::None,
    }
}

/// senpi's `setCapabilities` mutates the module-level cache read by `getCapabilities`; todo 9
/// owns that cache. This shim has no cache to mutate (every call to [`get_capabilities`]
/// returns the same fixed value), so this is a documented no-op until todo 9 lands.
pub fn set_capabilities(_capabilities: TerminalCapabilities) {}
