use std::{path::{Path, PathBuf}, sync::Arc};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use crate::{definition::*, filesystem_policy::*, model_only_text::model_only_text, truncate::*};
pub trait ReadOperations: Send + Sync {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>>;
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
    fn detect_image_mime_type<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
pub struct LocalReadOperations;
impl ReadOperations for LocalReadOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> { Box::pin(async move { Ok(tokio::fs::read(path).await?) }) }
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> { Box::pin(async move { tokio::fs::File::open(path).await?; Ok(()) }) }
    fn detect_image_mime_type<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Option<String>> { Box::pin(async move {
        use tokio::io::AsyncReadExt;
        let mut file = tokio::fs::File::open(path).await?; let mut bytes = vec![0;4100];
        let count = file.read(&mut bytes).await?; bytes.truncate(count);
        Ok(detect_supported_image_mime_type(&bytes).map(str::to_owned))
    }) }
}
pub struct ReadSummaryInput<'a> { pub path: &'a Path, pub text: &'a str, pub offset: Option<usize>, pub limit: Option<usize>, pub truncated: bool }
pub trait ReadFolder: Send + Sync {
    fn summarize<'a>(&'a self, input: ReadSummaryInput<'a>) -> ToolFuture<'a, Option<String>>;
}
#[derive(Clone, Default)]
pub struct ReadToolOptions {
    pub folder: Option<Arc<dyn ReadFolder>>, pub auto_resize_images: Option<bool>,
    pub operations: Option<Arc<dyn ReadOperations>>, pub filesystem_policy: Option<FilesystemPolicyChecker>,
}
#[derive(Deserialize)]
pub struct ReadToolInput { pub path: String, pub offset: Option<usize>, pub limit: Option<usize> }
pub fn detect_supported_image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[255,216,255]) { return if bytes.get(3) == Some(&247) { None } else { Some("image/jpeg") }; }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.get(8..16) != Some(&b"\0\0\0\rIHDR"[..]) { return None; }
        let mut offset = 8;
        while offset+8 <= bytes.len() {
            let length = u32::from_be_bytes([bytes[offset],bytes[offset+1],bytes[offset+2],bytes[offset+3]]) as usize;
            if bytes.get(offset+4..offset+8) == Some(&b"acTL"[..]) { return None; }
            if bytes.get(offset+4..offset+8) == Some(&b"IDAT"[..]) { break; }
            offset = offset.saturating_add(length).saturating_add(12);
        }
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF") { return Some("image/gif"); }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) { return Some("image/webp"); }
    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let uint = |i| u32::from_le_bytes([bytes[i],bytes[i+1],bytes[i+2],bytes[i+3]]);
        let size = uint(2); let pixel = uint(10); let dib = uint(14);
        if (size != 0 && size < 26) || pixel < 14u32.saturating_add(dib) || (size != 0 && pixel >= size) { return None; }
        let offset = if dib == 12 { 22 } else if (40..=124).contains(&dib) && bytes.len() >= 30 { 26 } else { return None; };
        let planes = u16::from_le_bytes([bytes[offset],bytes[offset+1]]); let bits = u16::from_le_bytes([bytes[offset+2],bytes[offset+3]]);
        if planes == 1 && [1,4,8,16,24,32].contains(&bits) { return Some("image/bmp"); }
    }
    None
}
fn process_image(bytes: Vec<u8>, mime: String, resize: bool) -> Result<(Vec<ToolContent>, String), ToolError> {
    let mut bytes = bytes; let mut mime = mime.split(';').next().unwrap_or(&mime).trim().to_lowercase(); let mut hints = Vec::new();
    if !["image/png","image/jpeg","image/jpg","image/gif","image/webp"].contains(&mime.as_str()) {
        let converted = image::load_from_memory(&bytes).and_then(|image| {
            let mut out = std::io::Cursor::new(Vec::new()); image.write_to(&mut out, image::ImageFormat::Png)?; Ok(out.into_inner())
        });
        match converted { Ok(data) => { hints.push(format!("[Image converted from {mime} to image/png.]")); bytes = data; mime = "image/png".into(); },
            Err(_) => return Ok((Vec::new(), "[Image omitted: could not be converted to a supported inline image format.]".into())) }
    }
    if mime == "image/jpg" { mime = "image/jpeg".into(); }
    if resize {
        let decoded = (|| -> image::ImageResult<image::DynamicImage> {
            use image::ImageDecoder;
            let mut decoder = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?.into_decoder()?;
            let orientation = decoder.orientation()?;
            let mut decoded = image::DynamicImage::from_decoder(decoder)?;
            decoded.apply_orientation(orientation);
            Ok(decoded)
        })();
        let decoded = match decoded { Ok(image) => image, Err(_) => return Ok((Vec::new(), "[Image omitted: could not be resized below the inline image size limit.]".into())) };
        let (width,height) = (decoded.width(),decoded.height());
        if width > 2000 || height > 2000 || bytes.len().div_ceil(3)*4 >= 4_718_592 {
            let (mut w,mut h) = (width,height);
            if w > 2000 { h = ((u64::from(h)*2000 + u64::from(w)/2)/u64::from(w)) as u32; w = 2000; }
            if h > 2000 { w = ((u64::from(w)*2000 + u64::from(h)/2)/u64::from(h)) as u32; h = 2000; }
            loop {
                let resized = decoded.resize_exact(w.max(1),h.max(1),image::imageops::FilterType::Lanczos3);
                let mut out = std::io::Cursor::new(Vec::new());
                resized.write_to(&mut out,image::ImageFormat::Png).map_err(|e| ToolError::Message(e.to_string()))?;
                let mut candidates = vec![(out.into_inner(), "image/png")];
                for quality in [80,85,70,55,40] {
                    let mut data = Vec::new(); image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality).encode_image(&resized.to_rgb8()).map_err(|e| ToolError::Message(e.to_string()))?;
                    candidates.push((data,"image/jpeg"));
                }
                if let Some((data,kind)) = candidates.into_iter().find(|(data,_)| data.len().div_ceil(3)*4 < 4_718_592) {
                    bytes = data; mime = kind.into(); hints.push(format!("[Image: original {width}x{height}, displayed at {w}x{h}. Multiply coordinates by {:.2} to map to original image.]", f64::from(width)/f64::from(w))); break;
                }
                if w == 1 && h == 1 { return Ok((Vec::new(), "[Image omitted: could not be resized below the inline image size limit.]".into())); }
                w = (w*3/4).max(1); h = (h*3/4).max(1);
            }
        }
    }
    let mut note = format!("Read image file [{mime}]"); if !hints.is_empty() { note.push('\n'); note.push_str(&hints.join("\n")); }
    Ok((vec![ToolContent::Image { data: base64::engine::general_purpose::STANDARD.encode(bytes), mime_type: mime }], note))
}
pub fn create_read_tool_definition(cwd: PathBuf, options: ReadToolOptions) -> ToolDefinition {
    let ops = options.operations.clone().unwrap_or_else(|| Arc::new(LocalReadOperations));
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let options = options.clone(); let ops = Arc::clone(&ops);
        Box::pin(async move {
            let input: ReadToolInput = serde_json::from_value(call.params)?;
            if input.path.to_ascii_lowercase().starts_with("local://") { return Err(ToolError::Message("local:// URIs resolve only inside eval cells via the kernel read()/write() helpers; the read tool takes filesystem paths. Re-read this with the eval read() helper, or retry with the plain absolute file path.".into())); }
            call.signal.check()?;
            let work = async {
                let path = crate::path_utils::resolve_read_path_async(&input.path, call.context.map_or(cwd.as_path(), ToolContext::cwd)).await;
                check_filesystem_policy(options.filesystem_policy.as_ref(), &path, FilesystemOperation::Read, "read").await?;
                ops.access(&path).await?;
                let mime = ops.detect_image_mime_type(&path).await?; let buffer = ops.read_file(&path).await?;
                if let Some(mime) = mime {
                    let original_mime = mime.clone(); let resize = options.auto_resize_images.unwrap_or(true);
                    let (mut images, mut note) = tokio::task::spawn_blocking(move || process_image(buffer, mime, resize)).await.map_err(|e| ToolError::Message(e.to_string()))??;
                    if images.is_empty() { note = format!("Read image file [{original_mime}]\n{note}"); }
                    if call.context.and_then(ToolContext::model).is_some_and(|m| !m.input.contains(&maho_ai::types::InputModality::Image)) { note.push_str("\n[Current model does not support images. The image will be omitted from this request.]"); }
                    let mut content = vec![ToolContent::text(note)]; content.append(&mut images); return Ok(ToolResult { content, details: None });
                }
                let text = String::from_utf8_lossy(&buffer); let lines: Vec<_> = text.split('\n').collect(); let start = input.offset.unwrap_or(1).saturating_sub(1);
                if start >= lines.len() { return Err(ToolError::Message(format!("Offset {} is beyond end of file ({} lines total)", input.offset.unwrap_or(1), lines.len()))); }
                let end = input.limit.map_or(lines.len(), |limit| start.saturating_add(limit).min(lines.len()));
                let truncation = truncate_head(&lines[start..end].join("\n"), TruncationOptions::default());
                let summary = if let Some(folder) = &options.folder { folder.summarize(ReadSummaryInput { path: &path, text: &text, offset: input.offset, limit: input.limit, truncated: truncation.truncated }).await? } else { None };
                let mut details = None; let mut notice = None;
                let output = if let Some(summary) = summary { summary }
                else if truncation.first_line_exceeds_limit {
                    notice = Some(format!("[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]", start+1, format_size(lines[start].len()), format_size(DEFAULT_MAX_BYTES), start+1, input.path, DEFAULT_MAX_BYTES)); details = Some(json!({"truncation":truncation})); String::new()
                } else if truncation.truncated {
                    let display_end = start+truncation.output_lines;
                    notice = Some(format!("[Showing lines {}-{display_end} of {}{}. Use offset={} to continue.]", start+1, lines.len(), if truncation.truncated_by.as_deref() == Some("lines") { String::new() } else { format!(" ({} limit)",format_size(DEFAULT_MAX_BYTES)) }, display_end+1));
                    details = Some(json!({"truncation":truncation})); truncation.content
                } else {
                    if input.limit.is_some() && end < lines.len() { notice = Some(format!("[{} more lines in file. Use offset={} to continue.]",lines.len()-end,end+1)); }
                    truncation.content
                };
                let mut content = Vec::new(); if !output.is_empty() || notice.is_none() { content.push(ToolContent::text(format!("{output}{}",if notice.is_some() { "\n" } else { "" }))); }
                if let Some(notice) = notice { content.push(model_only_text(notice)); } Ok(ToolResult { content, details })
            };
            tokio::select! { result = work => result, () = call.signal.cancelled() => Err(ToolError::Aborted) }
        })
    });
    let mut tool = ToolDefinition::new("read", "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.", json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"number"},"limit":{"type":"number"}},"required":["path"]}), execute);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(maho_ai::types::ConstrainedSamplingConfig::JsonSchema { strict: maho_ai::types::JsonSchemaStrictness::Prefer }));
    tool.prompt_snippet = Some("Read file contents".into()); tool.prompt_guidelines = Some(vec!["Use read to examine files instead of cat or sed.".into()]); tool
}
