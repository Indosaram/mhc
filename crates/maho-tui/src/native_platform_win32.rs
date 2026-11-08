//! Port of senpi `packages/tui/native/win32/src/win32-platform.c`, the Windows native platform
//! helper: `ENABLE_VIRTUAL_TERMINAL_INPUT` on the console input handle, and the Win32 clipboard
//! for text and images (PNG, else a CF_DIBV5/CF_DIB bitmap wrapped in a BMP file header).
//! Registered by [`crate::native_platform::register_builtin_native_platform_helper`].
//!
//! `isModifierPressed` (`GetAsyncKeyState`) and the console output setup (`SetConsoleOutputCP`
//! plus `ENABLE_VIRTUAL_TERMINAL_PROCESSING`) live here too; each Win32 call is a narrow
//! `#[allow(unsafe_code)]` block with a SAFETY note.
// SAFETY (module-wide): the only `unsafe` here is the documented Win32 calls below.
#![allow(unsafe_code)]

use clipboard_win::{
    formats, get_clipboard, get_clipboard_string, is_format_avail, register_format,
    set_clipboard_string,
};
use crossterm_winapi::{ConsoleMode, Handle, HandleType};

use crate::native_platform::{ModifierKey, NativeClipboard, NativePlatformHelper};

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
        let color_count = if bits_per_pixel <= 8 {
            1usize << bits_per_pixel
        } else {
            0
        };
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
        get_clipboard_string()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn get_image(&self) -> Result<Option<Vec<u8>>, String> {
        let png_format = register_format("PNG")
            .map(std::num::NonZeroU32::get)
            .filter(|format| is_format_avail(*format));
        let dib_format = [formats::CF_DIBV5, formats::CF_DIB]
            .into_iter()
            .find(|format| is_format_avail(*format));
        let Some(format) = png_format.or(dib_format) else {
            return Ok(None);
        };
        let bytes: Vec<u8> =
            get_clipboard(formats::RawData(format)).map_err(|error| error.to_string())?;
        if bytes.is_empty() {
            return Err("Clipboard does not contain an image".to_string());
        }
        if png_format.is_some() {
            return Ok(Some(bytes));
        }
        dib_to_bmp(&bytes)
            .map(Some)
            .ok_or_else(|| "Could not create clipboard image buffer".to_string())
    }

    fn set_text(&self, text: &str) -> Option<Result<(), String>> {
        Some(set_clipboard_string(text).map_err(|error| error.to_string()))
    }
}

impl NativePlatformHelper for Win32PlatformHelper {
    fn enable_virtual_terminal_input(&self) -> Option<bool> {
        let enabled = Handle::new(HandleType::InputHandle)
            .map(ConsoleMode::from)
            .and_then(|console| {
                let mode = console.mode()?;
                console.set_mode(mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
            });
        Some(enabled.is_ok())
    }

    /// Enable UTF-8 output and ANSI processing, remembering the previous console state so
    /// [`Self::restore_console_output`] can put it back when the TUI releases the terminal.
    fn enable_virtual_terminal_output(&self) -> Option<bool> {
        let saved = console_output::enable();
        let enabled = saved.is_some();
        if let Some(saved) = saved {
            *console_output::SAVED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(saved);
        }
        Some(enabled)
    }

    fn restore_console_output(&self) {
        let saved = console_output::SAVED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(saved) = saved {
            console_output::restore(saved);
        }
    }

    fn is_modifier_pressed(&self, key: ModifierKey) -> Option<bool> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
        };
        let virtual_key = match key {
            ModifierKey::Shift => VK_SHIFT,
            ModifierKey::Control => VK_CONTROL,
            ModifierKey::Option => VK_MENU,
            ModifierKey::Command => VK_LWIN,
        };
        // SAFETY: GetAsyncKeyState takes an integer virtual-key code and only reports key state;
        // it has no pointer arguments and cannot fail in a way that affects memory.
        let pressed = unsafe { GetAsyncKeyState(virtual_key as i32) };
        let pressed = pressed as u16 & 0x8000 != 0;
        if matches!(key, ModifierKey::Command) && !pressed {
            // SAFETY: same contract as above.
            let right = unsafe { GetAsyncKeyState(VK_RWIN as i32) };
            return Some(right as u16 & 0x8000 != 0);
        }
        Some(pressed)
    }
}

/// Console output state the TUI changes and puts back on exit.
#[cfg(windows)]
mod console_output {
    use std::sync::Mutex;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetConsoleOutputCP, STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleOutputCP,
    };

    const CP_UTF8: u32 = 65001;
    pub(super) const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    pub(super) const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    #[derive(Clone, Copy)]
    pub(super) struct Saved {
        codepage: u32,
        mode: Option<u32>,
    }

    pub(super) static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

    fn stdout_handle() -> Option<*mut std::ffi::c_void> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::GetStdHandle;
        // SAFETY: GetStdHandle has no preconditions; the returned handle is owned by the process.
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(handle)
        }
    }

    pub(super) fn enable() -> Option<Saved> {
        let codepage = {
            // SAFETY: SetConsoleOutputCP takes an integer codepage; no pointers are involved.
            let previous = unsafe { GetConsoleOutputCP() };
            // SAFETY: as above.
            unsafe { SetConsoleOutputCP(CP_UTF8) };
            previous
        };
        let mode = stdout_handle().and_then(|handle| {
            let mut mode = 0u32;
            // SAFETY: `handle` is the process's stdout console handle and `mode` is a valid
            // out-pointer for the duration of the call.
            let ok = unsafe { GetConsoleMode(handle, &mut mode) };
            if ok == 0 {
                return None;
            }
            let wanted = mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            // SAFETY: as above; `wanted` is a valid console mode mask.
            (unsafe { SetConsoleMode(handle, wanted) } != 0).then_some(mode)
        });
        (codepage != 0 || mode.is_some()).then_some(Saved { codepage, mode })
    }

    pub(super) fn restore(saved: Saved) {
        if saved.codepage != 0 {
            // SAFETY: SetConsoleOutputCP takes an integer codepage; no pointers are involved.
            unsafe { SetConsoleOutputCP(saved.codepage) };
        }
        if let (Some(handle), Some(mode)) = (stdout_handle(), saved.mode) {
            // SAFETY: `handle` is the process's stdout console handle; `mode` is a mask this
            // module read from that same handle.
            unsafe { SetConsoleMode(handle, mode) };
        }
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
