//! Port of senpi `packages/tui/native/darwin/src/darwin-platform.m`, the macOS native platform
//! helper: modifier state through `CGEventSourceFlagsState` and the general `NSPasteboard` for
//! clipboard text and images. Registered by
//! [`crate::native_platform::register_builtin_native_platform_helper`].

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSPasteboard;
use objc2_core_graphics::{CGEventFlags, CGEventSource, CGEventSourceStateID};
use objc2_foundation::{NSArray, NSString};

use crate::native_platform::{ModifierKey, NativeClipboard, NativePlatformHelper};

// The UTI values of `NSPasteboardTypeString`/`PNG`/`TIFF` (the AppKit statics are extern
// statics, which this crate cannot read without `unsafe`).
const TEXT_TYPE: &str = "public.utf8-plain-text";
const PNG_TYPE: &str = "public.png";
const TIFF_TYPE: &str = "public.tiff";

pub struct DarwinPlatformHelper;

fn modifier_mask(key: ModifierKey) -> CGEventFlags {
    match key {
        ModifierKey::Shift => CGEventFlags::MaskShift,
        ModifierKey::Command => CGEventFlags::MaskCommand,
        ModifierKey::Control => CGEventFlags::MaskControl,
        ModifierKey::Option => CGEventFlags::MaskAlternate,
    }
}

/// senpi's TIFF fallback re-encodes the pasteboard bitmap as PNG.
fn tiff_to_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use tiff::decoder::{Decoder, DecodingResult};
    let unsupported = || "Clipboard does not contain an image".to_string();
    let mut decoder = Decoder::new(std::io::Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions().map_err(|error| error.to_string())?;
    let color = match decoder.colortype().map_err(|error| error.to_string())? {
        tiff::ColorType::RGBA(8) => png::ColorType::Rgba,
        tiff::ColorType::RGB(8) => png::ColorType::Rgb,
        tiff::ColorType::GrayA(8) => png::ColorType::GrayscaleAlpha,
        tiff::ColorType::Gray(8) => png::ColorType::Grayscale,
        _ => return Err(unsupported()),
    };
    let DecodingResult::U8(pixels) = decoder.read_image().map_err(|error| error.to_string())? else {
        return Err(unsupported());
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    writer.write_image_data(&pixels).map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;
    Ok(out)
}

impl NativeClipboard for DarwinPlatformHelper {
    fn get_text(&self) -> Result<Option<String>, String> {
        autoreleasepool(|_| {
            let pasteboard = NSPasteboard::generalPasteboard();
            Ok(pasteboard.stringForType(&NSString::from_str(TEXT_TYPE)).map(|text| text.to_string()))
        })
    }

    fn get_image(&self) -> Result<Option<Vec<u8>>, String> {
        autoreleasepool(|_| {
            let pasteboard = NSPasteboard::generalPasteboard();
            let png_type = NSString::from_str(PNG_TYPE);
            let tiff_type = NSString::from_str(TIFF_TYPE);
            let types = NSArray::from_slice(&[&*png_type, &*tiff_type]);
            if pasteboard.availableTypeFromArray(&types).is_none() {
                return Ok(None);
            }
            if let Some(png) = pasteboard.dataForType(&png_type) {
                return Ok(Some(png.to_vec()));
            }
            let tiff = pasteboard
                .dataForType(&tiff_type)
                .ok_or_else(|| "Clipboard does not contain an image".to_string())?;
            tiff_to_png(&tiff.to_vec()).map(Some)
        })
    }

    fn set_text(&self, text: &str) -> Option<Result<(), String>> {
        Some(autoreleasepool(|_| {
            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            if pasteboard.setString_forType(&NSString::from_str(text), &NSString::from_str(TEXT_TYPE)) {
                Ok(())
            } else {
                Err("Could not set clipboard text".to_string())
            }
        }))
    }
}

impl NativePlatformHelper for DarwinPlatformHelper {
    fn is_modifier_pressed(&self, key: ModifierKey) -> Option<bool> {
        let flags = CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState);
        Some(flags.contains(modifier_mask(key)))
    }
}
