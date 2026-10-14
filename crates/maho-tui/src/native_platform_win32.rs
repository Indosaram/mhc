//! Port of senpi `packages/tui/native/win32/src/win32-platform.c`, the Windows native platform
//! helper: `ENABLE_VIRTUAL_TERMINAL_INPUT` on the console input handle, and the Win32 clipboard
//! for text and images (PNG, else a CF_DIBV5/CF_DIB bitmap wrapped in a BMP file header).
//! Registered by [`crate::native_platform::register_builtin_native_platform_helper`].
//!
//! `isModifierPressed` (`GetAsyncKeyState`) has no safe binding and this crate forbids `unsafe`,
//! so it keeps the trait default (`None`): callers fall back exactly as when senpi's addon is
//! missing.

use clipboard_win::{formats, get_clipboard, get_clipboard_string, is_format_avail, register_format, set_clipboard_string};
use crossterm_winapi::{ConsoleMode, Handle, HandleType};

use crate::native_platform::{NativeClipboard, NativePlatformHelper};

const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
const BITMAP_FILE_HEADER_SIZE: usize = 14;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

pub struct Win32PlatformHelper;

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// senpi's `dib_pixel_offset`: the pixel-array offset inside a packed DIB, or `None` when the
/// header is malformed.
fn dib_pixel_offset(dib: &[u8]) -> Option<usize> {
    let size = dib.len();
    let header_size = read_u32(dib, 0)? as usize;
    if header_size == 12 {
        let bits_per_pixel = read_u16(dib, 10)?;
        let color_count = if bits_per_pixel <= 8 { 1usize << bits_per_pixel } else { 0 };
        let offset = header_size + color_count * 3;
        return (offset <= size).then_some(offset);
    }
    if header_size < 40 || header_size > size {
        return None;
    }
    let bits_per_pixel = read_u16(dib, 14)?;
    let compression = read_u32(dib, 16)?;
    let mut color_count = read_u32(dib, 32)? as usize;
    if color_count == 0 && bits_per_pixel <= 8 {
        color_count = 1usize << bits_per_pixel;
    }
    let mut offset = header_size + color_count * 4;
    if header_size == 40 && compression == BI_BITFIELDS {
        offset += 3 * 4;
    }
    if header_size == 40 && compression == BI_ALPHABITFIELDS {
        offset += 4 * 4;
    }
    (offset <= size).then_some(offset)
}

/// Prefix a packed DIB with a `BITMAPFILEHEADER`, producing a `.bmp` file.
fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    let offset = dib_pixel_offset(dib)?;
    let total = u32::try_from(dib.len() + BITMAP_FILE_HEADER_SIZE).ok()?;
    let pixel_start = u32::try_from(BITMAP_FILE_HEADER_SIZE + offset).ok()?;
    let mut bitmap = Vec::with_capacity(dib.len() + BITMAP_FILE_HEADER_SIZE);
    bitmap.extend_from_slice(b"BM");
    bitmap.extend_from_slice(&total.to_le_bytes());
    bitmap.extend_from_slice(&[0; 4]);
    bitmap.extend_from_slice(&pixel_start.to_le_bytes());
    bitmap.extend_from_slice(dib);
    Some(bitmap)
}

impl NativeClipboard for Win32PlatformHelper {
    fn get_text(&self) -> Result<Option<String>, String> {
        if !is_format_avail(formats::CF_UNICODETEXT) {
            return Ok(None);
        }
        get_clipboard_string().map(Some).map_err(|error| error.to_string())
    }

    fn get_image(&self) -> Result<Option<Vec<u8>>, String> {
        let png_format = register_format("PNG").map(std::num::NonZeroU32::get).filter(|format| is_format_avail(*format));
        let dib_format = [formats::CF_DIBV5, formats::CF_DIB].into_iter().find(|format| is_format_avail(*format));
        let Some(format) = png_format.or(dib_format) else {
            return Ok(None);
        };
        let bytes: Vec<u8> = get_clipboard(formats::RawData(format)).map_err(|error| error.to_string())?;
        if bytes.is_empty() {
            return Err("Clipboard does not contain an image".to_string());
        }
        if png_format.is_some() {
            return Ok(Some(bytes));
        }
        dib_to_bmp(&bytes).map(Some).ok_or_else(|| "Could not create clipboard image buffer".to_string())
    }

    fn set_text(&self, text: &str) -> Option<Result<(), String>> {
        Some(set_clipboard_string(text).map_err(|error| error.to_string()))
    }
}

impl NativePlatformHelper for Win32PlatformHelper {
    fn enable_virtual_terminal_input(&self) -> Option<bool> {
        let enabled = Handle::new(HandleType::InputHandle).map(ConsoleMode::from).and_then(|console| {
            let mode = console.mode()?;
            console.set_mode(mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
        });
        Some(enabled.is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_is_wrapped_in_a_bitmap_file_header_pointing_at_the_pixels() {
        let mut dib = vec![0u8; 44];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        let bmp = dib_to_bmp(&dib).expect("bmp");
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(read_u32(&bmp, 2), Some(58));
        assert_eq!(read_u32(&bmp, 10), Some(54));
        assert_eq!(&bmp[14..], dib.as_slice());
    }
}
