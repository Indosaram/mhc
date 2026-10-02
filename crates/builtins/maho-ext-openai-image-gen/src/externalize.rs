use std::{fs::{self, OpenOptions}, io::Write, path::Path};
use maho_ai::types::{AssistantMessage, ContentBlock, TextContent};
use maho_ext_imagegen::paths::{display_path, sanitize_image_stem, GENERATED_IMAGE_DIRECTORY};
use serde_json::Value;

pub const IMAGE_GENERATION_CALL_SUBTYPE: &str = "image_generation_call";
pub struct CompletedNativeImage<'a> { pub id: Option<&'a str>, pub result: &'a str, pub revised_prompt: Option<&'a str> }
pub fn read_completed_native_image(raw: &Value) -> Option<CompletedNativeImage<'_>> {
    if raw.get("type")?.as_str()? != IMAGE_GENERATION_CALL_SUBTYPE || raw.get("status")?.as_str()? != "completed" { return None; }
    let result = raw.get("result")?.as_str()?;
    if result.is_empty() { return None; }
    Some(CompletedNativeImage { id: raw.get("id").and_then(Value::as_str), result, revised_prompt: raw.get("revised_prompt").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) })
}
pub fn detect_image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) { "jpg" }
    else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" { "webp" }
    else { "png" }
}
// Node Buffer's base64 decoder accepts both alphabets and ignores nonalphabet bytes.
fn decode_base64(result: &str) -> Vec<u8> {
    let mut output = Vec::new();
    let mut bits = 0u32;
    let mut count = 0;
    for byte in result.bytes() {
        let value = match byte { b'A'..=b'Z' => byte-b'A', b'a'..=b'z' => byte-b'a'+26, b'0'..=b'9' => byte-b'0'+52, b'+'|b'-' => 62, b'/'|b'_' => 63, b'=' => break, _ => continue };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 { count -= 8; output.push((bits >> count) as u8); }
    }
    output
}
fn externalize_block(cwd: &Path, image: CompletedNativeImage<'_>, response_id: Option<&str>, index: usize) -> Result<String, String> {
    let bytes = decode_base64(image.result);
    if bytes.is_empty() { return Err("decoded to zero bytes".into()); }
    let fallback = format!("{}-{index}", response_id.unwrap_or("image"));
    let stem = sanitize_image_stem(image.id.unwrap_or(&fallback));
    let extension = detect_image_extension(&bytes);
    let directory = cwd.join(GENERATED_IMAGE_DIRECTORY);
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    for attempt in 1..=100 {
        let suffix = if attempt == 1 { String::new() } else { format!("-{attempt}") };
        let target = directory.join(format!("{stem}{suffix}.{extension}"));
        match OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(mut file) => {
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                let mut text = format!("Generated image: {}", display_path(cwd, &target));
                if let Some(prompt) = image.revised_prompt { text.push_str(&format!("\nRevised prompt: {prompt}")); }
                return Ok(text);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!("no free filename for {stem}.{extension}"))
}
pub fn externalize_native_images(message: &AssistantMessage, cwd: &Path) -> Option<AssistantMessage> {
    let mut result = message.clone();
    let mut replaced = false;
    for (index, block) in result.content.iter_mut().enumerate() {
        let image = match block { ContentBlock::ProviderNative(native) if native.subtype == IMAGE_GENERATION_CALL_SUBTYPE => read_completed_native_image(&native.raw), _ => None };
        let Some(image) = image else { continue; };
        let text = externalize_block(cwd, image, message.response_id.as_deref(), index).unwrap_or_else(|reason| format!("Generated image could not be saved: {reason}."));
        *block = ContentBlock::Text(TextContent { text, ..Default::default() });
        replaced = true;
    }
    replaced.then_some(result)
}
